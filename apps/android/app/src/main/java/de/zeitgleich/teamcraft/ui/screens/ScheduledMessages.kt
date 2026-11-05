package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TimePicker
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.material3.rememberTimePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ScheduledMessage
import de.zeitgleich.teamcraft.core.ScheduledOverview
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.ScheduleField
import de.zeitgleich.teamcraft.ui.ScheduleTimes
import de.zeitgleich.teamcraft.ui.forms.FieldErrors
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.focusFirst
import de.zeitgleich.teamcraft.ui.forms.supportingText
import java.time.Instant
import java.time.LocalDate
import java.time.LocalTime
import java.time.ZoneId
import java.time.ZoneOffset

/** The overview, read again whenever the store changes. */
@Composable
private fun rememberScheduledOverview(viewModel: MainViewModel): ScheduledOverview? {
  val changes by viewModel.changeCount.collectAsState()
  var overview by remember { mutableStateOf<ScheduledOverview?>(null) }
  LaunchedEffect(changes) {
    overview = viewModel.readChat { query(ChatQueries.scheduledMessages()).overview }
  }
  return overview
}

/** Everything waiting to be sent, soonest first, each with what happens next. */
@Composable
fun ScheduledScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  val maybeOverview = rememberScheduledOverview(viewModel)
  var maybeEditing by remember { mutableStateOf<ScheduledMessage?>(null) }
  var maybeCancelling by remember { mutableStateOf<ScheduledMessage?>(null) }
  Scaffold(
    topBar = { ChromeTopBar(title = stringResource(R.string.scheduled_title), maybeOnBack = onBack) },
    contentWindowInsets = ScreenInsets,
  ) { padding ->
    LazyColumn(Modifier.fillMaxSize().padding(padding)) {
      item {
        Text(
          stringResource(R.string.scheduled_note),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(16.dp),
        )
      }
      val messages = maybeOverview?.messages.orEmpty()
      if (messages.isEmpty()) item { EmptyText(R.string.scheduled_empty) }
      items(messages, key = { it.id }) { message ->
        ScheduledRow(
          message = message,
          onOpen = onOpen,
          onSendNow = { viewModel.act(ChatActions.sendScheduledNow(message.id)) },
          onEdit = { maybeEditing = message },
          onCancel = { maybeCancelling = message },
        )
        HorizontalDivider()
      }
    }
  }
  val editing = maybeEditing
  if (editing != null && maybeOverview != null) {
    ScheduledEditor(
      message = editing,
      overview = maybeOverview,
      onSave = { body, dueAt ->
        viewModel.act(ChatActions.updateScheduled(editing.id, body, dueAt))
        maybeEditing = null
      },
      onDismiss = { maybeEditing = null },
    )
  }
  maybeCancelling?.let { message ->
    AlertDialog(
      onDismissRequest = { maybeCancelling = null },
      title = {
        Text(stringResource(if (message.isRemoved) R.string.scheduled_dismiss_question else R.string.scheduled_cancel_question))
      },
      confirmButton = {
        TextButton(onClick = {
          viewModel.act(ChatActions.cancelScheduled(message.id))
          maybeCancelling = null
        }) { Text(stringResource(if (message.isRemoved) R.string.scheduled_dismiss_confirm else R.string.scheduled_cancel_confirm)) }
      },
      dismissButton = { TextButton(onClick = { maybeCancelling = null }) { Text(stringResource(R.string.scheduled_keep)) } },
    )
  }
}

@Composable
private fun ScheduledRow(
  message: ScheduledMessage,
  onOpen: (String) -> Unit,
  onSendNow: () -> Unit,
  onEdit: () -> Unit,
  onCancel: () -> Unit,
) {
  val context = LocalContext.current
  Column(Modifier.fillMaxWidth().padding(16.dp)) {
    Text(
      message.conversationName.ifEmpty { stringResource(R.string.scheduled_unknown_conversation) },
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.primary,
    )
    Text(
      DateUtils.formatDateTime(
        context,
        message.dueAt,
        DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_ABBREV_ALL,
      ),
      fontWeight = FontWeight.Bold,
    )
    Text(
      stringResource(statusText(message.status)),
      style = MaterialTheme.typography.bodySmall,
      color = if (message.needsAttention) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
    )
    message.maybeRemovesAt?.takeIf { !message.isRemoved }?.let { removesAt ->
      Text(
        stringResource(
          R.string.scheduled_removal_date,
          DateUtils.formatDateTime(context, removesAt, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_WEEKDAY),
        ),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.error,
      )
    }
    // Selectable: for a message the server removed, copying the text is all that is left to do.
    SelectionContainer {
      Text(
        message.body,
        maxLines = if (message.isRemoved) Int.MAX_VALUE else 4,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.padding(vertical = 4.dp),
      )
    }
    Row(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
      if (message.canGoOut) TextButton(onClick = onSendNow) { Text(stringResource(R.string.scheduled_send_now)) }
      if (message.canEdit) TextButton(onClick = onEdit) { Text(stringResource(R.string.scheduled_edit)) }
      if (message.canGoOut) {
        TextButton(onClick = { onOpen(message.conversation) }) { Text(stringResource(R.string.scheduled_open)) }
      }
      TextButton(onClick = onCancel) {
        Text(stringResource(if (message.isRemoved) R.string.scheduled_dismiss else R.string.scheduled_cancel))
      }
    }
  }
}

private fun statusText(status: String): Int = when (status) {
  ScheduledMessage.STATUS_WAITING -> R.string.scheduled_status_waiting
  ScheduledMessage.STATUS_NEEDS_SEALING -> R.string.scheduled_status_needs_sealing
  ScheduledMessage.STATUS_STALE -> R.string.scheduled_status_stale
  ScheduledMessage.STATUS_POLICY_OFF -> R.string.scheduled_status_policy_off
  ScheduledMessage.STATUS_NOT_A_MEMBER -> R.string.scheduled_status_not_a_member
  ScheduledMessage.STATUS_NO_PERMISSION -> R.string.scheduled_status_no_permission
  ScheduledMessage.STATUS_CONVERSATION_GONE -> R.string.scheduled_status_conversation_gone
  ScheduledMessage.STATUS_THREAD_GONE -> R.string.scheduled_status_thread_gone
  ScheduledMessage.STATUS_UNREADABLE -> R.string.scheduled_status_unreadable
  ScheduledMessage.STATUS_REMOVED -> R.string.scheduled_status_removed
  else -> R.string.scheduled_status_unknown
}

/** Changes the text and time of a waiting message. */
@Composable
private fun ScheduledEditor(
  message: ScheduledMessage,
  overview: ScheduledOverview,
  onSave: (String, Long) -> Unit,
  onDismiss: () -> Unit,
) {
  var body by remember { mutableStateOf(message.body) }
  var dueAt by remember { mutableLongStateOf(message.dueAt) }
  var errors by remember { mutableStateOf(FieldErrors.of<ScheduleField>()) }
  var wasRefused by remember { mutableStateOf(false) }
  val bodyFocus = remember { FocusRequester() }
  val timeFocus = remember { FocusRequester() }
  fun follow() {
    if (wasRefused) errors = ScheduleTimes.errors(body, dueAt, overview)
  }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.scheduled_edit)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        OutlinedTextField(
          value = body,
          onValueChange = {
            body = it
            follow()
          },
          label = { Text(stringResource(R.string.scheduled_edit_text)) },
          isError = errors[ScheduleField.BODY] != null,
          supportingText = supportingText(errors[ScheduleField.BODY]),
          modifier = Modifier.fillMaxWidth().focusRequester(bodyFocus).fieldError(errors[ScheduleField.BODY]),
        )
        DueAtField(
          dueAtMs = dueAt,
          focus = timeFocus,
          maybeError = errors[ScheduleField.TIME],
          onChange = {
            dueAt = it
            follow()
          },
        )
      }
    },
    confirmButton = {
      TextButton(onClick = {
        val found = ScheduleTimes.errors(body, dueAt, overview)
        if (found.isValid) {
          onSave(body, dueAt)
        } else {
          errors = found
          wasRefused = true
          found.focusFirst(mapOf(ScheduleField.BODY to bodyFocus, ScheduleField.TIME to timeFocus))
        }
      }) { Text(stringResource(R.string.scheduled_save)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** A date and a time, each on its own button, with the field's error below them. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DueAtField(dueAtMs: Long, focus: FocusRequester, maybeError: Int?, onChange: (Long) -> Unit) {
  val context = LocalContext.current
  val zone = ZoneId.systemDefault()
  val shown = Instant.ofEpochMilli(dueAtMs).atZone(zone)
  var isPickingDate by remember { mutableStateOf(false) }
  var isPickingTime by remember { mutableStateOf(false) }
  Column(Modifier.fieldError(maybeError)) {
    Text(stringResource(R.string.schedule_custom_label), style = MaterialTheme.typography.labelMedium)
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      OutlinedButton(onClick = { isPickingDate = true }, modifier = Modifier.focusRequester(focus)) {
        Text(DateUtils.formatDateTime(context, dueAtMs, DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_ABBREV_ALL))
      }
      OutlinedButton(onClick = { isPickingTime = true }) {
        Text(DateUtils.formatDateTime(context, dueAtMs, DateUtils.FORMAT_SHOW_TIME))
      }
    }
    maybeError?.let { error ->
      Text(stringResource(error), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.error)
    }
  }
  if (isPickingDate) {
    val state = rememberDatePickerState(
      initialSelectedDateMillis = shown.toLocalDate().atStartOfDay(ZoneOffset.UTC).toInstant().toEpochMilli(),
    )
    DatePickerDialog(
      onDismissRequest = { isPickingDate = false },
      confirmButton = {
        TextButton(
          onClick = {
            state.selectedDateMillis?.let { picked ->
              val date: LocalDate = Instant.ofEpochMilli(picked).atZone(ZoneOffset.UTC).toLocalDate()
              onChange(ScheduleTimes.at(date, shown.toLocalTime(), zone))
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
  if (isPickingTime) {
    val state = rememberTimePickerState(initialHour = shown.hour, initialMinute = shown.minute)
    AlertDialog(
      onDismissRequest = { isPickingTime = false },
      text = { TimePicker(state = state) },
      confirmButton = {
        TextButton(onClick = {
          onChange(ScheduleTimes.at(shown.toLocalDate(), LocalTime.of(state.hour, state.minute), zone))
          isPickingTime = false
        }) { Text(stringResource(R.string.ok)) }
      },
      dismissButton = { TextButton(onClick = { isPickingTime = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}

/**
 * "Send later" in the composer's "+" sheet: the row that opens the choices. Absent where the
 * workspace doesn't allow scheduling; with nothing to schedule or with files attached it says why.
 */
@Composable
fun SendLaterRow(viewModel: MainViewModel, canSchedule: Boolean, hasFiles: Boolean, onOpen: () -> Unit) {
  val maybeOverview = rememberScheduledOverview(viewModel)
  if (maybeOverview?.policy?.isEnabled != true) return
  HorizontalDivider(Modifier.padding(vertical = 8.dp))
  Column(
    (if (canSchedule) Modifier.clickable(onClick = onOpen) else Modifier)
      .fillMaxWidth()
      .padding(horizontal = 24.dp, vertical = 8.dp),
  ) {
    Text(
      stringResource(R.string.schedule_heading),
      style = MaterialTheme.typography.titleSmall,
      color = if (canSchedule) MaterialTheme.colorScheme.onSurface else MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Text(
      stringResource(
        when {
          hasFiles -> R.string.schedule_no_files
          !canSchedule -> R.string.schedule_write_first
          else -> R.string.schedule_online_hint
        },
      ),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** The times to choose from. `onPick` gets the time; the composer does the rest. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SendLaterSheet(viewModel: MainViewModel, onPick: (Long) -> Unit, onDismiss: () -> Unit) {
  val context = LocalContext.current
  val maybeOverview = rememberScheduledOverview(viewModel)
  val overview = maybeOverview ?: return
  val zone = ZoneId.systemDefault()
  val nowMs = System.currentTimeMillis()
  var customAt by remember { mutableLongStateOf(ScheduleTimes.inOneHour(overview.serverNow)) }
  var maybeCustomError by remember { mutableStateOf<Int?>(null) }
  val customFocus = remember { FocusRequester() }
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp)) {
      Text(
        stringResource(R.string.schedule_heading),
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(horizontal = 24.dp),
      )
      Text(
        stringResource(R.string.schedule_online_hint),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
      )
      listOf(
        R.string.schedule_in_one_hour to ScheduleTimes.inOneHour(overview.serverNow),
        R.string.schedule_tomorrow to ScheduleTimes.tomorrowMorning(nowMs, zone),
        R.string.schedule_monday to ScheduleTimes.mondayMorning(nowMs, zone),
      ).filter { (_, time) -> ScheduleTimes.isWithinRules(time, overview) }.forEach { (label, time) ->
        Column(Modifier.fillMaxWidth().clickable { onPick(time) }.padding(horizontal = 24.dp, vertical = 12.dp)) {
          Text(stringResource(label))
          Text(
            DateUtils.formatDateTime(
              context,
              time,
              DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_TIME or DateUtils.FORMAT_SHOW_WEEKDAY or DateUtils.FORMAT_ABBREV_ALL,
            ),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
      }
      HorizontalDivider(Modifier.padding(vertical = 8.dp))
      Column(Modifier.padding(horizontal = 24.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
        DueAtField(
          dueAtMs = customAt,
          focus = customFocus,
          maybeError = maybeCustomError,
          onChange = {
            customAt = it
            if (maybeCustomError != null) maybeCustomError = ScheduleTimes.timeError(it, overview)
          },
        )
        OutlinedButton(onClick = {
          val maybeError = ScheduleTimes.timeError(customAt, overview)
          if (maybeError == null) {
            onPick(customAt)
          } else {
            maybeCustomError = maybeError
            customFocus.requestFocus()
          }
        }) { Text(stringResource(R.string.schedule_confirm)) }
      }
    }
  }
}

/** On the main screen while a scheduled message waits for the user: nothing waits silently. */
@Composable
fun ScheduledAttentionCard(viewModel: MainViewModel, onOpen: () -> Unit) {
  val maybeOverview = rememberScheduledOverview(viewModel)
  val count = maybeOverview?.messages?.count { it.needsAttention } ?: 0
  if (count == 0) return
  Card(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp).clickable(onClick = onOpen)) {
    Text(stringResource(R.string.scheduled_attention), modifier = Modifier.padding(12.dp))
  }
}
