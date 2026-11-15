package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.annotation.StringRes
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberDatePickerState
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
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.error
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
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
import uniffi.teamcraft_ffi.MeetingExternalInvitee
import uniffi.teamcraft_ffi.MeetingForm
import uniffi.teamcraft_ffi.MeetingFormProblem
import uniffi.teamcraft_ffi.MeetingRepeat
import uniffi.teamcraft_ffi.MeetingSave
import uniffi.teamcraft_ffi.SavedMeeting
import uniffi.teamcraft_ffi.MeetingSeriesEnd
import uniffi.teamcraft_ffi.meetingFormProblems
import uniffi.teamcraft_ffi.meetingFormWithRepeat
import uniffi.teamcraft_ffi.newMeetingForm
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.time.ZoneOffset
import java.time.format.TextStyle

/** The lengths the form offers, as on desktop; a saved meeting's own length is offered too. */
private val MEETING_LENGTHS = listOf(15u, 30u, 45u, 60u, 90u, 120u)

/** The repeat choices, in the order desktop offers them. */
private val REPEATS = listOf(
  MeetingRepeat.NEVER to R.string.meeting_repeat_never,
  MeetingRepeat.DAILY to R.string.meeting_repeat_daily,
  MeetingRepeat.WEEKLY to R.string.meeting_repeat_weekly,
  MeetingRepeat.MONTHLY to R.string.meeting_repeat_monthly,
)

/** The fields of the form an error can belong to, in their order on the sheet. */
internal enum class MeetingField { TITLE, START, LENGTH, EXTERNALS, INTERVAL, WEEKDAYS, COUNT, UNTIL }

internal val MeetingFormProblem.field: MeetingField
  get() = when (this) {
    MeetingFormProblem.MissingTitle -> MeetingField.TITLE
    MeetingFormProblem.StartInThePast -> MeetingField.START
    MeetingFormProblem.InvalidDuration -> MeetingField.LENGTH
    is MeetingFormProblem.InvalidEmail -> MeetingField.EXTERNALS
    MeetingFormProblem.InvalidInterval -> MeetingField.INTERVAL
    MeetingFormProblem.NoWeekday, MeetingFormProblem.StartOffWeekdays -> MeetingField.WEEKDAYS
    MeetingFormProblem.InvalidCount -> MeetingField.COUNT
    MeetingFormProblem.InvalidUntil, MeetingFormProblem.UntilBeforeStart -> MeetingField.UNTIL
  }

@get:StringRes
internal val MeetingFormProblem.text: Int
  get() = when (this) {
    MeetingFormProblem.MissingTitle -> R.string.meeting_error_title
    MeetingFormProblem.StartInThePast -> R.string.meeting_error_past
    MeetingFormProblem.InvalidDuration -> R.string.meeting_error_duration
    is MeetingFormProblem.InvalidEmail -> R.string.meeting_error_email
    MeetingFormProblem.InvalidInterval -> R.string.meeting_error_interval
    MeetingFormProblem.NoWeekday -> R.string.meeting_error_no_weekday
    MeetingFormProblem.StartOffWeekdays -> R.string.meeting_error_start_weekday
    MeetingFormProblem.InvalidCount -> R.string.meeting_error_count
    MeetingFormProblem.InvalidUntil -> R.string.meeting_error_until
    MeetingFormProblem.UntilBeforeStart -> R.string.meeting_error_until_before
  }

/** The user's offset from UTC at [atMs], in minutes: a series' days of the week are local days. */
internal fun utcOffsetMinutesAt(atMs: Long, zone: ZoneId = ZoneId.systemDefault()): Int =
  zone.rules.getOffset(Instant.ofEpochMilli(atMs)).totalSeconds / 60

/** [this] starting at [startsAtMs], with the offset that goes with it. */
internal fun MeetingForm.startingAt(startsAtMs: Long, zone: ZoneId = ZoneId.systemDefault()): MeetingForm =
  copy(startsAtMs = startsAtMs, utcOffsetMinutes = utcOffsetMinutesAt(startsAtMs, zone))

/** The last second of the local day [date]: a series that ends on it still meets that day. */
internal fun endOfDayMs(date: LocalDate, zone: ZoneId = ZoneId.systemDefault()): Long =
  date.atTime(23, 59, 59).atZone(zone).toInstant().toEpochMilli()

/**
 * Schedules a meeting ([maybeMeeting] `null`; listed in the space [maybeSpaceId]) or changes the user's own, with what desktop's dialog
 * has: title, description, start, length, invited people and e-mail addresses, co-hosts, the
 * notetaker, the repeat rule and the lobby. The checks are the core's (`meetingFormProblems`).
 * Errors show at their field once a save was refused, the first invalid field takes the focus
 * (forms.md); a failure the server answered stays above the button with the form as it is.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MeetingFormSheet(
  viewModel: MainViewModel,
  maybeMeeting: Meeting?,
  onSaved: (SavedMeeting) -> Unit,
  onDismiss: () -> Unit,
  maybeSpaceId: String? = null,
) {
  val changes by viewModel.changeCount.collectAsState()
  val scope = rememberCoroutineScope()
  var maybeForm by remember {
    mutableStateOf<MeetingForm?>(
      if (maybeMeeting == null) {
        val now = System.currentTimeMillis()
        newMeetingForm(now, utcOffsetMinutesAt(now)).let { it.startingAt(it.startsAtMs) }
      } else {
        null
      },
    )
  }
  var maybeSavedStart by remember { mutableStateOf<Long?>(null) }
  var isSavedSeries by remember { mutableStateOf(false) }
  var maybeNotetaker by remember { mutableStateOf<String?>(null) }
  // Names of the people invited, for the co-hosts: from the directory and from each pick.
  var names by remember { mutableStateOf(mapOf<String, String>()) }
  var isSaving by remember { mutableStateOf(false) }
  var wasRefused by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val focus = remember { MeetingField.entries.associateWith { FocusRequester() } }
  LaunchedEffect(Unit) {
    maybeNotetaker = viewModel.readChat { meetingNotetaker() }
    viewModel.readChat { snapshot() }?.let { snapshot -> names = snapshot.users.associate { it.id to it.displayName } + names }
  }
  LaunchedEffect(maybeMeeting?.id) {
    val meeting = maybeMeeting ?: return@LaunchedEffect
    when (val answer = viewModel.tryMeetingAction { meetingEditForm(meeting.id) }) {
      is ActionAnswer.Answered -> {
        // The core doesn't know the time zone: the offset is set here, for the saved start.
        maybeForm = answer.value.form.startingAt(answer.value.form.startsAtMs)
        maybeSavedStart = answer.value.form.startsAtMs
        isSavedSeries = answer.value.isRepeating
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
    val change: (MeetingForm) -> Unit = { maybeForm = it }
    val problems = if (wasRefused) meetingFormProblems(form, System.currentTimeMillis(), maybeSavedStart) else emptyList()
    fun errorOf(field: MeetingField): Int? = problems.firstOrNull { it.field == field }?.text
    val save: () -> Unit = save@{
      if (isSaving) return@save
      val found = meetingFormProblems(form, System.currentTimeMillis(), maybeSavedStart)
      if (found.isNotEmpty()) {
        wasRefused = true
        focus[found.minOf { it.field }]?.requestFocus()
        return@save
      }
      isSaving = true
      maybeFailure = null
      scope.launch {
        val answer = viewModel.tryMeetingAction {
          if (maybeMeeting == null) scheduleMeeting(form, maybeSpaceId) else saveMeetingEdit(maybeMeeting.id, form)
        }
        when (answer) {
          is ActionAnswer.Answered -> when (val outcome = answer.value) {
            is MeetingSave.Saved -> onSaved(outcome.meeting)
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
      if (isSavedSeries) {
        Text(
          stringResource(R.string.meeting_form_series_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(horizontal = Spacing.l),
        )
      }
      OutlinedTextField(
        value = form.title,
        onValueChange = { change(form.copy(title = it)) },
        label = { Text(stringResource(R.string.meeting_form_name)) },
        isError = errorOf(MeetingField.TITLE) != null,
        supportingText = supportingText(errorOf(MeetingField.TITLE)),
        singleLine = true,
        enabled = !isSaving,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l).focusRequester(focus.getValue(MeetingField.TITLE)),
      )
      OutlinedTextField(
        value = form.description,
        onValueChange = { change(form.copy(description = it)) },
        label = { Text(stringResource(R.string.meeting_form_description)) },
        minLines = 2,
        enabled = !isSaving,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l),
      )
      Column(Modifier.padding(horizontal = Spacing.l)) {
        DueAtField(
          dueAtMs = form.startsAtMs,
          focus = focus.getValue(MeetingField.START),
          maybeError = errorOf(MeetingField.START),
          onChange = { change(form.startingAt(it)) },
          label = R.string.meeting_form_start,
        )
      }
      LengthField(form, errorOf(MeetingField.LENGTH), focus.getValue(MeetingField.LENGTH), isSaving, change)
      Text(
        stringResource(R.string.meeting_form_invite),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(horizontal = Spacing.l),
      )
      maybeNotetaker?.let { bot -> NotetakerRow(form, bot, isSaving, change) }
      PeoplePicker(
        search = { query -> viewModel.readChat { searchPeople(query, PEOPLE_SHOWN) } },
        changes = changes,
        picked = form.inviteeUserIds,
        onPick = { user ->
          names = names + (user.id to user.displayName)
          val invitees = toggled(form.inviteeUserIds, user.id)
          change(form.copy(inviteeUserIds = invitees, coHostUserIds = form.coHostUserIds.filter { it in invitees }))
        },
        excluded = setOfNotNull(viewModel.maybeMyUserId, maybeNotetaker),
        isEnabled = !isSaving,
        modifier = Modifier.heightIn(max = 320.dp),
      )
      if (form.inviteeUserIds.isNotEmpty()) CoHostsSection(form, names, isSaving, change)
      ExternalInviteesSection(form, problems, focus.getValue(MeetingField.EXTERNALS), isSaving, change)
      HorizontalDivider()
      RepeatSection(form, isSavedSeries, ::errorOf, focus, isSaving, change)
      HorizontalDivider()
      Row(
        Modifier.fillMaxWidth().padding(horizontal = Spacing.l),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.m),
      ) {
        Text(stringResource(R.string.meeting_form_lobby), modifier = Modifier.weight(1f))
        Switch(checked = form.hasLobby, onCheckedChange = { change(form.copy(hasLobby = it)) }, enabled = !isSaving)
      }
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

@Composable
private fun LengthField(form: MeetingForm, maybeError: Int?, focus: FocusRequester, isSaving: Boolean, change: (MeetingForm) -> Unit) {
  Column(Modifier.padding(horizontal = Spacing.l).fieldError(maybeError).focusRequester(focus).focusable()) {
    Text(stringResource(R.string.meeting_form_duration), style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
      (MEETING_LENGTHS + form.durationMinutes).distinct().sorted().forEach { minutes ->
        FilterChip(
          selected = form.durationMinutes == minutes,
          onClick = { change(form.copy(durationMinutes = minutes)) },
          label = { Text(stringResource(R.string.meeting_form_minutes, minutes.toInt())) },
          enabled = !isSaving,
        )
      }
    }
    FieldErrorText(maybeError)
  }
}

/** "Notetaker joins the calls" (e2ee.md §8.2): invited like a member, asked into every call. */
@Composable
internal fun NotetakerRow(form: MeetingForm, bot: String, isSaving: Boolean, change: (MeetingForm) -> Unit) {
  val isTicked = form.maybeNotetakerUserId != null
  Row(
    Modifier
      .fillMaxWidth()
      .toggleable(value = isTicked, role = Role.Checkbox, enabled = !isSaving) {
        change(form.copy(maybeNotetakerUserId = if (it) bot else null))
      }
      .padding(horizontal = Spacing.l, vertical = Spacing.s),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Checkbox(checked = isTicked, onCheckedChange = null, enabled = !isSaving)
    Column(Modifier.weight(1f).padding(start = Spacing.m)) {
      Text(stringResource(R.string.meeting_form_notetaker))
      Text(
        stringResource(R.string.meeting_form_notetaker_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
  }
}

/** Who hosts with the organizer, among the invited members (#1720); this device signs them. */
@Composable
internal fun CoHostsSection(form: MeetingForm, names: Map<String, String>, isSaving: Boolean, change: (MeetingForm) -> Unit) {
  Column {
    Text(
      stringResource(R.string.meeting_form_co_hosts),
      style = MaterialTheme.typography.titleSmall,
      modifier = Modifier.padding(horizontal = Spacing.l),
    )
    form.inviteeUserIds.forEach { id ->
      val isCoHost = id in form.coHostUserIds
      Row(
        Modifier
          .fillMaxWidth()
          .toggleable(value = isCoHost, role = Role.Checkbox, enabled = !isSaving) {
            change(form.copy(coHostUserIds = toggled(form.coHostUserIds, id)))
          }
          .padding(horizontal = Spacing.l, vertical = Spacing.xs),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        Checkbox(checked = isCoHost, onCheckedChange = null, enabled = !isSaving)
        Text(names[id] ?: stringResource(R.string.meeting_form_unknown_person), modifier = Modifier.padding(start = Spacing.m))
      }
    }
    Text(
      stringResource(R.string.meeting_form_co_hosts_hint),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = Spacing.l),
    )
  }
}

/** People without an account, invited by e-mail address; each address that can't be one is named. */
@Composable
internal fun ExternalInviteesSection(
  form: MeetingForm,
  problems: List<MeetingFormProblem>,
  focus: FocusRequester,
  isSaving: Boolean,
  change: (MeetingForm) -> Unit,
) {
  var email by remember { mutableStateOf("") }
  var name by remember { mutableStateOf("") }
  val badEmails = problems.filterIsInstance<MeetingFormProblem.InvalidEmail>().map { it.email }
  val maybeErrorText = badEmails.firstOrNull()?.let { stringResource(R.string.meeting_error_email, it) }
  val add = {
    if (email.isNotBlank()) {
      change(form.copy(externalInvitees = form.externalInvitees + MeetingExternalInvitee(email.trim(), name.trim())))
      email = ""
      name = ""
    }
  }
  Column(
    Modifier.padding(horizontal = Spacing.l).semantics { if (maybeErrorText != null) error(maybeErrorText) },
    verticalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    Text(stringResource(R.string.meeting_form_external), style = MaterialTheme.typography.titleSmall)
    form.externalInvitees.forEachIndexed { index, external ->
      val isBad = external.email.trim() in badEmails
      Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
          if (external.name.isBlank()) external.email else "${external.name} <${external.email}>",
          color = if (isBad) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
          modifier = Modifier.weight(1f),
        )
        val removeLabel = stringResource(R.string.meeting_form_external_remove, external.email)
        IconButton(
          onClick = { change(form.copy(externalInvitees = form.externalInvitees.filterIndexed { at, _ -> at != index })) },
          enabled = !isSaving,
          modifier = Modifier.semantics { contentDescription = removeLabel },
        ) { Icon(Icons.Filled.Close, contentDescription = null) }
      }
    }
    OutlinedTextField(
      value = email,
      onValueChange = { email = it },
      label = { Text(stringResource(R.string.meeting_form_external_email)) },
      isError = maybeErrorText != null,
      singleLine = true,
      enabled = !isSaving,
      keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Email, imeAction = ImeAction.Next),
      modifier = Modifier.fillMaxWidth().focusRequester(focus),
    )
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
      OutlinedTextField(
        value = name,
        onValueChange = { name = it },
        label = { Text(stringResource(R.string.meeting_form_external_name)) },
        singleLine = true,
        enabled = !isSaving,
        modifier = Modifier.weight(1f),
      )
      OutlinedButton(onClick = add, enabled = !isSaving && email.isNotBlank()) { Text(stringResource(R.string.meeting_form_add)) }
    }
    maybeErrorText?.let { text ->
      Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
  }
}

/** How the meeting repeats: every N days, weeks or months, on which days, and until when. */
@Composable
internal fun RepeatSection(
  form: MeetingForm,
  isSavedSeries: Boolean,
  errorOf: (MeetingField) -> Int?,
  focus: Map<MeetingField, FocusRequester>,
  isSaving: Boolean,
  change: (MeetingForm) -> Unit,
) {
  val repeat = form.repeatRule
  Column(Modifier.padding(horizontal = Spacing.l), verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
    Text(stringResource(R.string.meeting_form_repeat), style = MaterialTheme.typography.titleSmall)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
      REPEATS.forEach { (choice, label) ->
        FilterChip(
          selected = repeat.frequency == choice,
          onClick = { runCatching { meetingFormWithRepeat(form, choice) }.onSuccess(change) },
          label = { Text(stringResource(label)) },
          // A series stays a series when edited (its end is cancelling the following meetings).
          enabled = !isSaving && !(isSavedSeries && choice == MeetingRepeat.NEVER),
        )
      }
    }
    if (repeat.frequency == MeetingRepeat.NEVER) return@Column
    NumberWithUnit(
      label = R.string.meeting_form_every,
      value = repeat.interval,
      unit = when (repeat.frequency) {
        MeetingRepeat.DAILY -> R.plurals.meeting_unit_days
        MeetingRepeat.WEEKLY -> R.plurals.meeting_unit_weeks
        else -> R.plurals.meeting_unit_months
      },
      maybeError = errorOf(MeetingField.INTERVAL),
      focus = focus.getValue(MeetingField.INTERVAL),
      isEnabled = !isSaving,
      onChange = { change(form.copy(repeatRule = repeat.copy(interval = it))) },
    )
    if (repeat.frequency == MeetingRepeat.DAILY || repeat.frequency == MeetingRepeat.WEEKLY) {
      WeekdayChips(form, errorOf(MeetingField.WEEKDAYS), focus.getValue(MeetingField.WEEKDAYS), isSaving, change)
    }
    Row(
      Modifier
        .fillMaxWidth()
        .toggleable(value = repeat.skipsHolidays, role = Role.Checkbox, enabled = !isSaving) {
          change(form.copy(repeatRule = repeat.copy(skipsHolidays = it)))
        },
      verticalAlignment = Alignment.CenterVertically,
    ) {
      Checkbox(checked = repeat.skipsHolidays, onCheckedChange = null, enabled = !isSaving)
      Column(Modifier.weight(1f).padding(start = Spacing.m)) {
        Text(stringResource(R.string.meeting_form_skip_holidays))
        Text(
          stringResource(R.string.meeting_form_skip_holidays_hint),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
      }
    }
    SeriesEndChoice(form, errorOf, focus, isSaving, change)
  }
}

/** "Every [2] weeks": a number with its unit, which follows the number's plural. */
@Composable
private fun NumberWithUnit(
  @StringRes label: Int,
  value: UInt,
  unit: Int,
  maybeError: Int?,
  focus: FocusRequester,
  isEnabled: Boolean,
  onChange: (UInt) -> Unit,
) {
  Column {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
      Text(stringResource(label))
      NumberInput(value, maybeError, focus, isEnabled, onChange)
      Text(pluralStringResource(unit, value.toInt(), value.toInt()))
    }
    FieldErrorText(maybeError)
  }
}

/** A whole number from the keyboard's digits; an empty field is 0, which the core refuses. */
@Composable
private fun NumberInput(value: UInt, maybeError: Int?, focus: FocusRequester, isEnabled: Boolean, onChange: (UInt) -> Unit) {
  OutlinedTextField(
    value = if (value == 0u) "" else value.toString(),
    onValueChange = { typed -> onChange(typed.filter(Char::isDigit).take(4).toUIntOrNull() ?: 0u) },
    isError = maybeError != null,
    singleLine = true,
    enabled = isEnabled,
    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number, imeAction = ImeAction.Done),
    modifier = Modifier.width(88.dp).focusRequester(focus).fieldError(maybeError),
  )
}

/** The days of the week a daily or weekly series meets on, Monday first, as toggle chips. */
@Composable
private fun WeekdayChips(form: MeetingForm, maybeError: Int?, focus: FocusRequester, isSaving: Boolean, change: (MeetingForm) -> Unit) {
  val locale = LocalContext.current.resources.configuration.locales[0]
  Column(Modifier.fieldError(maybeError).focusRequester(focus).focusable()) {
    Text(stringResource(R.string.meeting_form_on_days), style = MaterialTheme.typography.labelMedium)
    FlowRow(horizontalArrangement = Arrangement.spacedBy(Spacing.xs)) {
      DayOfWeek.values().forEachIndexed { index, day ->
        val isOn = form.repeatRule.weekdays.getOrElse(index) { false }
        FilterChip(
          selected = isOn,
          onClick = {
            val days = form.repeatRule.weekdays.toMutableList().also { it[index] = !isOn }
            change(form.copy(repeatRule = form.repeatRule.copy(weekdays = days)))
          },
          label = { Text(day.getDisplayName(TextStyle.SHORT, locale)) },
          enabled = !isSaving,
        )
      }
    }
    FieldErrorText(maybeError)
  }
}

/** Never, after a number of meetings, or on a date; the count and the date belong to their choice. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SeriesEndChoice(
  form: MeetingForm,
  errorOf: (MeetingField) -> Int?,
  focus: Map<MeetingField, FocusRequester>,
  isSaving: Boolean,
  change: (MeetingForm) -> Unit,
) {
  val context = LocalContext.current
  val repeat = form.repeatRule
  var isPickingDate by remember { mutableStateOf(false) }
  val choose: (MeetingSeriesEnd) -> Unit = { change(form.copy(repeatRule = repeat.copy(ends = it))) }
  Column(Modifier.selectableGroup()) {
    Text(stringResource(R.string.meeting_form_ends), style = MaterialTheme.typography.labelMedium)
    EndRow(repeat.ends == MeetingSeriesEnd.NEVER, !isSaving, { choose(MeetingSeriesEnd.NEVER) }) {
      Text(stringResource(R.string.meeting_form_ends_never))
    }
    EndRow(repeat.ends == MeetingSeriesEnd.AFTER_COUNT, !isSaving, { choose(MeetingSeriesEnd.AFTER_COUNT) }) {
      Text(stringResource(R.string.meeting_form_ends_after))
      NumberInput(repeat.count, errorOf(MeetingField.COUNT), focus.getValue(MeetingField.COUNT), !isSaving) {
        change(form.copy(repeatRule = repeat.copy(ends = MeetingSeriesEnd.AFTER_COUNT, count = it)))
      }
      Text(pluralStringResource(R.plurals.meeting_unit_meetings, repeat.count.toInt(), repeat.count.toInt()))
    }
    FieldErrorText(errorOf(MeetingField.COUNT))
    EndRow(repeat.ends == MeetingSeriesEnd.ON_DATE, !isSaving, { choose(MeetingSeriesEnd.ON_DATE) }) {
      Text(stringResource(R.string.meeting_form_ends_on))
      OutlinedButton(
        onClick = { isPickingDate = true },
        enabled = !isSaving,
        modifier = Modifier.focusRequester(focus.getValue(MeetingField.UNTIL)).fieldError(errorOf(MeetingField.UNTIL)),
      ) {
        Text(
          repeat.maybeUntilMs?.let { DateUtils.formatDateTime(context, it, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_ABBREV_ALL) }
            ?: stringResource(R.string.meeting_form_pick_date),
        )
      }
    }
    FieldErrorText(errorOf(MeetingField.UNTIL))
  }
  if (isPickingDate) {
    val zone = ZoneId.systemDefault()
    val shown = Instant.ofEpochMilli(repeat.maybeUntilMs ?: form.startsAtMs).atZone(zone).toLocalDate()
    val state = rememberDatePickerState(initialSelectedDateMillis = shown.atStartOfDay(ZoneOffset.UTC).toInstant().toEpochMilli())
    DatePickerDialog(
      onDismissRequest = { isPickingDate = false },
      confirmButton = {
        TextButton(
          onClick = {
            state.selectedDateMillis?.let { picked ->
              val date = Instant.ofEpochMilli(picked).atZone(ZoneOffset.UTC).toLocalDate()
              change(form.copy(repeatRule = repeat.copy(ends = MeetingSeriesEnd.ON_DATE, maybeUntilMs = endOfDayMs(date, zone))))
            }
            isPickingDate = false
          },
          enabled = state.selectedDateMillis != null,
        ) { Text(stringResource(R.string.ok)) }
      },
      dismissButton = { TextButton(onClick = { isPickingDate = false }) { Text(stringResource(R.string.cancel)) } },
    ) {
      DatePicker(state = state)
    }
  }
}

@Composable
private fun EndRow(isChosen: Boolean, isEnabled: Boolean, onChoose: () -> Unit, content: @Composable () -> Unit) {
  Row(
    Modifier.fillMaxWidth().selectable(selected = isChosen, enabled = isEnabled, role = Role.RadioButton, onClick = onChoose),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
  ) {
    RadioButton(selected = isChosen, onClick = null, enabled = isEnabled)
    content()
  }
}

@Composable
private fun FieldErrorText(@StringRes maybeError: Int?) {
  maybeError?.let { error ->
    Text(stringResource(error), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
  }
}
