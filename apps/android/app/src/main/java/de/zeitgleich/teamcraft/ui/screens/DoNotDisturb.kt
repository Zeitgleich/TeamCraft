package de.zeitgleich.teamcraft.ui.screens

import de.zeitgleich.teamcraft.core.User
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.heading
import androidx.compose.runtime.collectAsState
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.foundation.layout.navigationBarsPadding
import android.app.DatePickerDialog
import android.app.TimePickerDialog
import android.content.Context
import android.text.format.DateFormat
import java.time.DayOfWeek
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import java.time.format.TextStyle
import java.util.Locale
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
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
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.ManualPresence
import de.zeitgleich.teamcraft.core.NotificationSettings
import de.zeitgleich.teamcraft.notifications.DndEditor
import de.zeitgleich.teamcraft.notifications.DndRange
import de.zeitgleich.teamcraft.notifications.QuickDnd
import de.zeitgleich.teamcraft.notifications.Weekday
import de.zeitgleich.teamcraft.ui.AppState
import de.zeitgleich.teamcraft.ui.MainViewModel

/**
 * Do not disturb, stored on the server so every device and the push routing agree: quick DND
 * through presence, the weekly schedule in this device's time zone, and who or what breaks through.
 */
@Composable
fun DoNotDisturbScreen(viewModel: MainViewModel, account: AppState.Ready, onBack: () -> Unit) {
  var maybeSettings by remember { mutableStateOf<NotificationSettings?>(null) }
  var editor by remember { mutableStateOf(DndEditor()) }
  var maybeSnapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var hasLoadFailed by remember { mutableStateOf(false) }
  LaunchedEffect(Unit) {
    val settings = viewModel.readChat { notificationSettings() }
    maybeSettings = settings
    hasLoadFailed = settings == null
    editor = DndEditor.from(settings?.maybeDndSchedule)
    maybeSnapshot = viewModel.readChat { snapshot() }
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.dnd_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(
      Modifier.fillMaxSize().padding(padding).verticalScroll(rememberScrollState()).padding(16.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      QuickDndSection(viewModel, account)
      HorizontalDivider()
      val settings = maybeSettings
      when {
        hasLoadFailed -> Text(stringResource(R.string.error_generic), color = MaterialTheme.colorScheme.error)
        settings == null -> Text(stringResource(R.string.loading))
        else -> ScheduleSection(viewModel, settings, editor, maybeSnapshot, onEdit = { editor = it }, onSaved = { maybeSettings = it })
      }
    }
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun QuickDndSection(viewModel: MainViewModel, account: AppState.Ready) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  var maybeState by remember { mutableStateOf<String?>(null) }
  var maybeUntilMs by remember { mutableStateOf<Long?>(null) }
  var hasFailed by remember { mutableStateOf(false) }
  LaunchedEffect(Unit) { maybeState = viewModel.readChat { presenceState(account.userId) } }
  fun set(manual: String, maybeUntil: Instant?) {
    hasFailed = false
    scope.launch {
      val done = viewModel.readChat { setManualPresence(ManualPresence(manual, maybeUntil?.toEpochMilli())) } != null
      hasFailed = !done
      if (done) {
        maybeState = if (manual == MANUAL_DND) MANUAL_DND else viewModel.readChat { presenceState(account.userId) }
        maybeUntilMs = maybeUntil?.toEpochMilli()
      }
    }
  }
  Text(stringResource(R.string.dnd_quick_title), style = MaterialTheme.typography.titleMedium)
  val isOn = maybeState == MANUAL_DND
  Text(
    when {
      !isOn -> stringResource(R.string.dnd_state_off)
      maybeUntilMs != null -> stringResource(R.string.dnd_state_until, formatDateTime(maybeUntilMs ?: 0))
      else -> stringResource(R.string.dnd_state_on)
    },
  )
  FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
    OutlinedButton(onClick = { set(MANUAL_DND, QuickDnd.forOneHour(Instant.now())) }) { Text(stringResource(R.string.dnd_one_hour)) }
    OutlinedButton(onClick = { set(MANUAL_DND, QuickDnd.untilMorning(Instant.now(), ZoneId.systemDefault())) }) {
      Text(stringResource(R.string.dnd_until_morning))
    }
    OutlinedButton(onClick = { pickDateTime(context) { set(MANUAL_DND, it) } }) { Text(stringResource(R.string.dnd_custom)) }
    if (isOn) Button(onClick = { set(MANUAL_AUTO, null) }) { Text(stringResource(R.string.dnd_end)) }
  }
  if (hasFailed) Text(stringResource(R.string.error_generic), color = MaterialTheme.colorScheme.error)
}

@Composable
private fun ScheduleSection(
  viewModel: MainViewModel,
  settings: NotificationSettings,
  editor: DndEditor,
  maybeSnapshot: ChatSnapshot?,
  onEdit: (DndEditor) -> Unit,
  onSaved: (NotificationSettings) -> Unit,
) {
  val scope = rememberCoroutineScope()
  var maybeSaveResult by remember { mutableStateOf<Boolean?>(null) }
  var isPickingPerson by remember { mutableStateOf(false) }
  // Names of people picked here: the directory may find people the snapshot doesn't know.
  var pickedNames by remember { mutableStateOf(mapOf<String, String>()) }
  Text(stringResource(R.string.dnd_schedule_title), style = MaterialTheme.typography.titleMedium)
  Text(stringResource(R.string.dnd_schedule_hint, ZoneId.systemDefault().id), style = MaterialTheme.typography.bodySmall)
  editor.ranges.forEachIndexed { index, range ->
    RangeRow(
      range = range,
      onChange = { changed -> onEdit(editor.copy(ranges = editor.ranges.toMutableList().also { it[index] = changed })) },
      onRemove = { onEdit(editor.copy(ranges = editor.ranges.filterIndexed { other, _ -> other != index })) },
    )
  }
  if (editor.canAddRange) {
    OutlinedButton(onClick = { onEdit(editor.copy(ranges = editor.ranges + DndRange.DEFAULT)) }) {
      Text(stringResource(R.string.dnd_add_range))
    }
  }

  HorizontalDivider()
  Text(stringResource(R.string.dnd_allow_title), style = MaterialTheme.typography.titleMedium)
  Text(stringResource(R.string.dnd_allow_hint), style = MaterialTheme.typography.bodySmall)
  for (userId in editor.allowUserIds) {
    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
      Text(pickedNames[userId] ?: maybeSnapshot?.name(userId) ?: userId, modifier = Modifier.weight(1f))
      TextButton(onClick = { onEdit(editor.copy(allowUserIds = editor.allowUserIds - userId)) }) {
        Text(stringResource(R.string.dnd_remove))
      }
    }
  }
  OutlinedButton(onClick = { isPickingPerson = true }) { Text(stringResource(R.string.dnd_add_person)) }
  Row(verticalAlignment = Alignment.CenterVertically) {
    Switch(checked = editor.allowsUrgent, onCheckedChange = { onEdit(editor.copy(allowsUrgent = it)) })
    Text(stringResource(R.string.dnd_allows_urgent), modifier = Modifier.padding(start = 8.dp))
  }

  Button(
    onClick = {
      val changed = settings.copy(maybeDndSchedule = editor.toSchedule(ZoneId.systemDefault(), Instant.now()))
      maybeSaveResult = null
      scope.launch {
        val isSaved = viewModel.readChat { setNotificationSettings(changed) } != null
        maybeSaveResult = isSaved
        if (isSaved) onSaved(changed)
      }
    },
    enabled = editor.isValid,
  ) { Text(stringResource(R.string.dnd_save)) }
  when (maybeSaveResult) {
    true -> Text(stringResource(R.string.dnd_saved))
    false -> Text(stringResource(R.string.error_generic), color = MaterialTheme.colorScheme.error)
    null -> Unit
  }

  if (isPickingPerson) {
    PersonPicker(
      viewModel = viewModel,
      excluded = editor.allowUserIds.toSet(),
      onPick = { user ->
        isPickingPerson = false
        pickedNames = pickedNames + (user.id to user.displayName)
        onEdit(editor.copy(allowUserIds = editor.allowUserIds + user.id))
      },
      onDismiss = { isPickingPerson = false },
    )
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun RangeRow(range: DndRange, onChange: (DndRange) -> Unit, onRemove: () -> Unit) {
  val context = LocalContext.current
  val locale = Locale.getDefault()
  Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
    FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
      for (day in Weekday.entries) {
        val isSelected = day in range.weekdays
        FilterChip(
          selected = isSelected,
          onClick = { onChange(range.copy(weekdays = if (isSelected) range.weekdays - day else range.weekdays + day)) },
          label = { Text(DayOfWeek.of(day.ordinal + 1).getDisplayName(TextStyle.SHORT, locale)) },
        )
      }
    }
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      TextButton(onClick = { pickTime(context, range.startMinute) { onChange(range.copy(startMinute = it)) } }) {
        Text(stringResource(R.string.dnd_from, formatMinute(range.startMinute)))
      }
      TextButton(onClick = { pickTime(context, range.endMinute) { onChange(range.copy(endMinute = it)) } }) {
        Text(stringResource(R.string.dnd_to, formatMinute(range.endMinute)))
      }
      TextButton(onClick = onRemove) { Text(stringResource(R.string.dnd_remove)) }
    }
    if (range.endMinute < range.startMinute) {
      Text(stringResource(R.string.dnd_next_day), style = MaterialTheme.typography.bodySmall)
    }
    if (!range.isValid) Text(stringResource(R.string.dnd_range_invalid), color = MaterialTheme.colorScheme.error)
  }
}

/** A person to let through do not disturb: the app's people picker, one tap picks (#1534). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun PersonPicker(viewModel: MainViewModel, excluded: Set<String>, onPick: (User) -> Unit, onDismiss: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    Text(
      stringResource(R.string.dnd_add_person),
      style = MaterialTheme.typography.titleLarge,
      modifier = Modifier.padding(horizontal = 16.dp).semantics { heading() },
    )
    PeoplePicker(
      search = { query -> viewModel.readChat { searchPeople(query, PEOPLE_SHOWN) } },
      changes = changes,
      picked = emptyList(),
      onPick = onPick,
      excluded = excluded,
      mode = PickMode.One,
      modifier = Modifier.heightIn(max = 520.dp).navigationBarsPadding(),
    )
  }
}

private fun pickTime(context: Context, minute: Int, onPicked: (Int) -> Unit) {
  TimePickerDialog(
    context,
    { _, hour, pickedMinute -> onPicked(hour * 60 + pickedMinute) },
    minute / 60,
    minute % 60,
    DateFormat.is24HourFormat(context),
  ).show()
}

/** A date, then a time; only moments in the future are taken. */
private fun pickDateTime(context: Context, onPicked: (Instant) -> Unit) {
  val today = LocalDate.now()
  DatePickerDialog(
    context,
    { _, year, month, day ->
      val date = LocalDate.of(year, month + 1, day)
      val now = LocalTime.now()
      TimePickerDialog(
        context,
        { _, hour, minute ->
          val chosen = date.atTime(hour, minute).atZone(ZoneId.systemDefault()).toInstant()
          if (chosen.isAfter(Instant.now())) onPicked(chosen)
        },
        now.hour,
        now.minute,
        DateFormat.is24HourFormat(context),
      ).show()
    },
    today.year,
    today.monthValue - 1,
    today.dayOfMonth,
  ).apply { datePicker.minDate = System.currentTimeMillis() }.show()
}

private fun formatMinute(minute: Int): String =
  LocalTime.of(minute / 60, minute % 60).format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))

private fun formatDateTime(ms: Long): String =
  Instant.ofEpochMilli(ms).atZone(ZoneId.systemDefault()).format(DateTimeFormatter.ofLocalizedDateTime(FormatStyle.SHORT))

private const val MANUAL_DND = "dnd"
private const val MANUAL_AUTO = "auto"
