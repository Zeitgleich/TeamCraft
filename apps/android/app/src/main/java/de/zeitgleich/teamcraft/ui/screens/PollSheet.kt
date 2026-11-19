package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import uniffi.teamcraft_ffi.PollDraftItem
import uniffi.teamcraft_ffi.PollFieldItem
import uniffi.teamcraft_ffi.PollProblemItem
import uniffi.teamcraft_ffi.PollProblemKind
import uniffi.teamcraft_ffi.PollResultsItem
import uniffi.teamcraft_ffi.PollLimitsItem
import uniffi.teamcraft_ffi.pollPostAction
import uniffi.teamcraft_ffi.pollProblems

/** Option fields a new poll starts with. */
private const val FIRST_OPTIONS = 2

private const val HOUR_MS = 3_600_000L

/** When the poll closes, as the sheet offers it. */
enum class PollClosing(@StringRes val label: Int, val afterMs: Long?) {
  NEVER(R.string.poll_closes_never, null),
  HOUR(R.string.poll_closes_hour, HOUR_MS),
  DAY(R.string.poll_closes_day, 24 * HOUR_MS),
  WEEK(R.string.poll_closes_week, 7 * 24 * HOUR_MS),
  AT(R.string.poll_closes_at, null),
}

/** When the poll shows its counts, as the sheet offers it. */
private val RESULTS = listOf(
  PollResultsItem.ALWAYS to R.string.poll_results_always,
  PollResultsItem.AFTER_VOTE to R.string.poll_results_after_vote,
  PollResultsItem.AFTER_CLOSE to R.string.poll_results_after_close,
)

/**
 * Makes a poll (#205), as the web dialog does: a question, two or more options, several answers
 * or one, whether it names who voted, when it shows counts and when it ends. The rules are the
 * core's (`poll_problems`): every error at its field at once, the first one focused on a refused
 * send, and from then on they follow the input (forms.md). "Hide who voted" says it isn't
 * anonymous (e2ee.md §4.12).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun PollSheet(viewModel: MainViewModel, conversationId: String, onDismiss: () -> Unit) {
  // The server's limits (#2120); the built-in ones show until they are read.
  var limits by remember { mutableStateOf(PollLimitsItem(12u, 300u, 100u, 365u)) }
  LaunchedEffect(Unit) { limits = viewModel.pollLimits() }
  var question by rememberSaveable { mutableStateOf("") }
  val options = remember { mutableStateListOf<String>().apply { repeat(FIRST_OPTIONS) { add("") } } }
  var isMultiple by rememberSaveable { mutableStateOf(false) }
  var hidesVoters by rememberSaveable { mutableStateOf(false) }
  var results by rememberSaveable { mutableStateOf(PollResultsItem.ALWAYS) }
  var closing by rememberSaveable { mutableStateOf(PollClosing.NEVER) }
  var closesAtMs by rememberSaveable { mutableStateOf(System.currentTimeMillis() + 24 * HOUR_MS) }
  var wasRefused by rememberSaveable { mutableStateOf(false) }
  val questionFocus = remember { FocusRequester() }
  val optionFocus = remember { mutableStateListOf<FocusRequester>().apply { repeat(FIRST_OPTIONS) { add(FocusRequester()) } } }
  val closingFocus = remember { FocusRequester() }
  // The text versions before polls show, in the sender's language.
  val fallbackTitle = stringResource(R.string.poll_title)
  val fallbackNote = stringResource(R.string.poll_fallback_note)

  fun draft(nowMs: Long) = PollDraftItem(
    question = question,
    options = options.toList(),
    isMultipleChoice = isMultiple,
    hidesVoters = hidesVoters,
    results = results,
    maybeClosesAtMs = when (closing) {
      PollClosing.NEVER -> null
      PollClosing.AT -> closesAtMs
      else -> closing.afterMs?.let { nowMs + it }
    },
  )

  // Errors show once a send was refused, and then follow the input.
  val problems = if (wasRefused) pollProblems(draft(System.currentTimeMillis()), limits, System.currentTimeMillis()) else emptyList()
  val questionError = problems.firstOrNull { it.field is PollFieldItem.Question }?.let { problemText(it) }
  val closingError = problems.firstOrNull { it.field is PollFieldItem.Closing }?.let { closingErrorText(it.kind) }

  fun send() {
    val nowMs = System.currentTimeMillis()
    val found = pollProblems(draft(nowMs), limits, nowMs)
    if (found.isNotEmpty()) {
      wasRefused = true
      when (val first = found.first().field) {
        is PollFieldItem.Question -> questionFocus.requestFocus()
        is PollFieldItem.Option -> optionFocus.getOrNull(first.index.toInt())?.requestFocus()
        is PollFieldItem.Closing -> closingFocus.requestFocus()
      }
      return
    }
    val action = runCatching {
      pollPostAction(conversationId, draft(nowMs), limits, fallbackTitle, fallbackNote, nowMs)
    }.getOrNull()
    if (action == null) {
      // The core refused what the check passed (a time that passed just now): show why.
      wasRefused = true
      return
    }
    viewModel.act(action)
    onDismiss()
  }

  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    Column(
      Modifier
        .fillMaxWidth()
        .verticalScroll(rememberScrollState())
        .imePadding()
        .navigationBarsPadding()
        .padding(horizontal = 24.dp)
        .padding(bottom = 24.dp),
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      Text(stringResource(R.string.poll_new_title), style = MaterialTheme.typography.titleLarge)
      OutlinedTextField(
        value = question,
        onValueChange = { question = it.take(limits.maxQuestionChars.toInt()) },
        label = { Text(stringResource(R.string.poll_question)) },
        isError = questionError != null,
        supportingText = questionError?.let { error -> { Text(error) } },
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, imeAction = ImeAction.Next),
        modifier = Modifier.fillMaxWidth().focusRequester(questionFocus),
      )
      Text(stringResource(R.string.poll_options), style = MaterialTheme.typography.titleSmall)
      options.forEachIndexed { index, option ->
        val maybeError = problems.firstOrNull { (it.field as? PollFieldItem.Option)?.index?.toInt() == index }?.let { problemText(it) }
        val number = index + 1
        Row(verticalAlignment = Alignment.Top) {
          OutlinedTextField(
            value = option,
            onValueChange = { options[index] = it.take(limits.maxOptionChars.toInt()) },
            label = { Text(stringResource(R.string.poll_option, number)) },
            isError = maybeError != null,
            supportingText = maybeError?.let { error -> { Text(error) } },
            singleLine = true,
            keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.Sentences, imeAction = ImeAction.Next),
            modifier = Modifier.weight(1f).focusRequester(optionFocus[index]),
          )
          if (options.size > FIRST_OPTIONS) {
            IconButton(
              onClick = {
                options.removeAt(index)
                optionFocus.removeAt(index)
              },
              modifier = Modifier.padding(top = 8.dp),
            ) {
              Icon(painterResource(R.drawable.ic_delete), contentDescription = stringResource(R.string.poll_remove_option, number))
            }
          }
        }
      }
      TextButton(
        onClick = {
          options.add("")
          optionFocus.add(FocusRequester())
        },
        enabled = options.size < limits.maxOptions.toInt(),
      ) { Text(stringResource(R.string.poll_add_option)) }
      SwitchRow(stringResource(R.string.poll_multiple), isMultiple) { isMultiple = it }
      SwitchRow(stringResource(R.string.poll_hide_voters), hidesVoters, hint = stringResource(R.string.poll_hide_voters_hint)) { hidesVoters = it }
      ChoiceGroup(
        title = stringResource(R.string.poll_results),
        choices = RESULTS.map { (mode, label) -> mode to stringResource(label) },
        selected = results,
        onSelect = { results = it },
      )
      ChoiceGroup(
        title = stringResource(R.string.poll_closes),
        choices = PollClosing.entries.map { it to stringResource(it.label) },
        selected = closing,
        onSelect = { closing = it },
      )
      if (closing == PollClosing.AT) {
        DueAtField(
          dueAtMs = closesAtMs,
          focus = closingFocus,
          maybeError = closingError,
          onChange = { closesAtMs = it },
          label = R.string.poll_closes_at_label,
        )
      }
      Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) }
        Spacer(Modifier.width(8.dp))
        Button(onClick = ::send) { Text(stringResource(R.string.poll_send)) }
      }
    }
  }
}

/** A switch with its label (and a hint under it); the whole row toggles. */
@Composable
private fun SwitchRow(label: String, isOn: Boolean, hint: String? = null, onChange: (Boolean) -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .toggleable(value = isOn, role = Role.Switch, onValueChange = onChange)
      .padding(vertical = 4.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Column(Modifier.weight(1f)) {
      Text(label, style = MaterialTheme.typography.bodyLarge)
      hint?.let { Text(it, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
    }
    Spacer(Modifier.width(12.dp))
    Switch(checked = isOn, onCheckedChange = null)
  }
}

/** Choices that exclude each other, as radio buttons under a title. */
@Composable
private fun <T> ChoiceGroup(title: String, choices: List<Pair<T, String>>, selected: T, onSelect: (T) -> Unit) {
  Column(Modifier.selectableGroup()) {
    Text(title, style = MaterialTheme.typography.titleSmall, modifier = Modifier.padding(bottom = 4.dp))
    for ((choice, label) in choices) {
      Row(
        Modifier
          .fillMaxWidth()
          .selectable(selected = choice == selected, role = Role.RadioButton, onClick = { onSelect(choice) })
          .padding(vertical = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        RadioButton(selected = choice == selected, onClick = null)
        Spacer(Modifier.width(8.dp))
        Text(label, style = MaterialTheme.typography.bodyLarge)
      }
    }
  }
}

/** What to do about a question's or option's problem, in the user's words. */
@Composable
private fun problemText(problem: PollProblemItem): String = when (val kind = problem.kind) {
  is PollProblemKind.EmptyQuestion -> stringResource(R.string.poll_error_question)
  is PollProblemKind.QuestionTooLong -> stringResource(R.string.poll_error_question_long, kind.max.toInt())
  is PollProblemKind.TooFewOptions -> stringResource(R.string.poll_error_options, kind.min.toInt())
  is PollProblemKind.TooManyOptions -> stringResource(R.string.poll_error_options_many, kind.max.toInt())
  is PollProblemKind.OptionTooLong -> stringResource(R.string.poll_error_option_long, kind.max.toInt())
  is PollProblemKind.DuplicateOption -> stringResource(R.string.poll_error_option_duplicate)
  is PollProblemKind.ClosingPassed -> stringResource(R.string.poll_error_closing_passed)
  is PollProblemKind.ClosingTooFar -> stringResource(R.string.poll_error_closing_far)
}

/** The closing time's problem as the date field shows it. */
@StringRes
private fun closingErrorText(kind: PollProblemKind): Int? = when (kind) {
  is PollProblemKind.ClosingPassed -> R.string.poll_error_closing_passed
  is PollProblemKind.ClosingTooFar -> R.string.poll_error_closing_far
  else -> null
}

/** The "+" sheet's row that opens [PollSheet]. */
@Composable
fun PollRow(onOpen: () -> Unit) {
  Row(
    Modifier
      .fillMaxWidth()
      .padding(bottom = 16.dp)
      .clickable(onClick = onOpen)
      .padding(horizontal = 24.dp, vertical = 12.dp),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(16.dp),
  ) {
    Icon(painterResource(R.drawable.ic_bar_chart), contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
    Column {
      Text(stringResource(R.string.poll_new_title), style = MaterialTheme.typography.titleSmall)
      Text(stringResource(R.string.poll_row_hint), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
    }
  }
}
