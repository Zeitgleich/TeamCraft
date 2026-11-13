package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.consumeWindowInsets
import android.text.format.DateUtils
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.Meeting
import de.zeitgleich.teamcraft.ui.ActionAnswer
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.ui.theme.Spacing
import de.zeitgleich.teamcraft.ui.toggled
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.MeetingCancelScope
import uniffi.teamcraft_ffi.MeetingForm
import uniffi.teamcraft_ffi.MeetingFormProblem
import uniffi.teamcraft_ffi.MeetingSave
import uniffi.teamcraft_ffi.meetingFormProblems
import uniffi.teamcraft_ffi.newMeetingForm
import uniffi.teamcraft_ffi.newRequestKey

/** The lengths the form offers, as on desktop; a saved meeting's own length is offered too. */
private val MEETING_LENGTHS = listOf(15u, 30u, 45u, 60u, 90u, 120u)

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

/** The fields of the form an error can belong to, in their order. */
private enum class MeetingField { TITLE, START, LENGTH }

private val MeetingFormProblem.field: MeetingField
  get() = when (this) {
    MeetingFormProblem.MISSING_TITLE -> MeetingField.TITLE
    MeetingFormProblem.START_IN_THE_PAST -> MeetingField.START
    MeetingFormProblem.INVALID_DURATION -> MeetingField.LENGTH
  }

@get:StringRes
private val MeetingFormProblem.text: Int
  get() = when (this) {
    MeetingFormProblem.MISSING_TITLE -> R.string.meeting_error_title
    MeetingFormProblem.START_IN_THE_PAST -> R.string.meeting_error_past
    MeetingFormProblem.INVALID_DURATION -> R.string.meeting_error_duration
  }

/**
 * Schedules a meeting ([maybeMeeting] `null`) or changes the user's own: title, description,
 * start, length, the lobby and the invited people. Errors show at their field once a save was
 * refused (forms.md); a failure the server answered stays above the button with the form as it is.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun MeetingFormSheet(viewModel: MainViewModel, maybeMeeting: Meeting?, onSaved: () -> Unit, onDismiss: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  val scope = rememberCoroutineScope()
  var maybeForm by remember { mutableStateOf<MeetingForm?>(if (maybeMeeting == null) newMeetingForm(System.currentTimeMillis()) else null) }
  var maybeSavedStart by remember { mutableStateOf<Long?>(null) }
  var isRepeating by remember { mutableStateOf(false) }
  var isSaving by remember { mutableStateOf(false) }
  var wasRefused by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val titleFocus = remember { FocusRequester() }
  val startFocus = remember { FocusRequester() }
  LaunchedEffect(maybeMeeting?.id) {
    val meeting = maybeMeeting ?: return@LaunchedEffect
    when (val answer = viewModel.tryMeetingAction { meetingEditForm(meeting.id) }) {
      is ActionAnswer.Answered -> {
        maybeForm = answer.value.form
        maybeSavedStart = answer.value.form.startsAtMs
        isRepeating = answer.value.isRepeating
      }
      is ActionAnswer.Failed -> {
        viewModel.showFailure(answer.text)
        onDismiss()
      }
    }
  }
  ModalBottomSheet(
    onDismissRequest = { if (!isSaving) onDismiss() },
    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
  ) {
    val form = maybeForm
    if (form == null) {
      CircularProgressIndicator(Modifier.padding(Spacing.xxl).align(Alignment.CenterHorizontally))
      return@ModalBottomSheet
    }
    val problems = if (wasRefused) meetingFormProblems(form, System.currentTimeMillis(), maybeSavedStart) else emptyList()
    fun errorOf(field: MeetingField): Int? = problems.firstOrNull { it.field == field }?.text
    val save: () -> Unit = save@{
      if (isSaving) return@save
      val found = meetingFormProblems(form, System.currentTimeMillis(), maybeSavedStart)
      if (found.isNotEmpty()) {
        wasRefused = true
        when (found.first().field) {
          MeetingField.TITLE -> titleFocus.requestFocus()
          MeetingField.START, MeetingField.LENGTH -> startFocus.requestFocus()
        }
        return@save
      }
      isSaving = true
      maybeFailure = null
      scope.launch {
        val answer = viewModel.tryMeetingAction {
          if (maybeMeeting == null) scheduleMeeting(form, null) else saveMeetingEdit(maybeMeeting.id, form)
        }
        when (answer) {
          is ActionAnswer.Answered -> when (answer.value) {
            is MeetingSave.Saved -> onSaved()
            is MeetingSave.Refused -> wasRefused = true
          }
          is ActionAnswer.Failed -> maybeFailure = answer.text
        }
        isSaving = false
      }
    }
    Column(
      Modifier.fillMaxWidth().imePadding().navigationBarsPadding().verticalScroll(rememberScrollState()).padding(bottom = Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.m),
    ) {
      Text(
        stringResource(if (maybeMeeting == null) R.string.meeting_form_title_new else R.string.meeting_form_title_edit),
        style = MaterialTheme.typography.titleLarge,
        modifier = Modifier.padding(horizontal = Spacing.l).semantics { heading() },
      )
      if (isRepeating) {
        Text(
          stringResource(R.string.meeting_form_series_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(horizontal = Spacing.l),
        )
      }
      OutlinedTextField(
        value = form.title,
        onValueChange = { maybeForm = form.copy(title = it) },
        label = { Text(stringResource(R.string.meeting_form_name)) },
        isError = errorOf(MeetingField.TITLE) != null,
        supportingText = supportingText(errorOf(MeetingField.TITLE)),
        singleLine = true,
        enabled = !isSaving,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l).focusRequester(titleFocus),
      )
      OutlinedTextField(
        value = form.description,
        onValueChange = { maybeForm = form.copy(description = it) },
        label = { Text(stringResource(R.string.meeting_form_description)) },
        minLines = 2,
        enabled = !isSaving,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l),
      )
      Column(Modifier.padding(horizontal = Spacing.l)) {
        DueAtField(
          dueAtMs = form.startsAtMs,
          focus = startFocus,
          maybeError = errorOf(MeetingField.START),
          onChange = { maybeForm = form.copy(startsAtMs = it) },
          label = R.string.meeting_form_start,
        )
      }
      Column(Modifier.padding(horizontal = Spacing.l).fieldError(errorOf(MeetingField.LENGTH))) {
        Text(stringResource(R.string.meeting_form_duration), style = MaterialTheme.typography.labelMedium)
        FlowRow(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
          (MEETING_LENGTHS + form.durationMinutes).distinct().sorted().forEach { minutes ->
            FilterChip(
              selected = form.durationMinutes == minutes,
              onClick = { maybeForm = form.copy(durationMinutes = minutes) },
              label = { Text(stringResource(R.string.meeting_form_minutes, minutes.toInt())) },
              enabled = !isSaving,
            )
          }
        }
        errorOf(MeetingField.LENGTH)?.let { error ->
          Text(stringResource(error), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
        }
      }
      Row(
        Modifier.fillMaxWidth().padding(horizontal = Spacing.l),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.m),
      ) {
        Text(stringResource(R.string.meeting_form_lobby), modifier = Modifier.weight(1f))
        Switch(checked = form.hasLobby, onCheckedChange = { maybeForm = form.copy(hasLobby = it) }, enabled = !isSaving)
      }
      Text(
        stringResource(R.string.meeting_form_invite),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(horizontal = Spacing.l),
      )
      PeoplePicker(
        search = { query -> viewModel.readChat { searchPeople(query, PEOPLE_SHOWN) } },
        changes = changes,
        picked = form.inviteeUserIds,
        onPick = { user -> maybeForm = form.copy(inviteeUserIds = toggled(form.inviteeUserIds, user.id)) },
        excluded = setOfNotNull(viewModel.maybeMyUserId),
        isEnabled = !isSaving,
        modifier = Modifier.heightIn(max = 320.dp),
      )
      // Why saving stopped, next to the button (forms.md).
      maybeFailure?.let { failure ->
        Text(
          stringResource(failure),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.padding(horizontal = Spacing.l).semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Button(onClick = save, enabled = !isSaving, modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l)) {
        if (isSaving) {
          CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
        } else {
          Text(stringResource(if (maybeMeeting == null) R.string.meeting_form_submit else R.string.meeting_form_save))
        }
      }
    }
  }
}
