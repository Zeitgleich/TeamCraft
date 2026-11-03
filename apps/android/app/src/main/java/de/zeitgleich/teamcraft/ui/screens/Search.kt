package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DatePicker
import androidx.compose.material3.DatePickerDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.InputChip
import androidx.compose.material3.InputChipDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberDatePickerState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import java.time.LocalDate
import kotlinx.coroutines.delay
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.SearchFilters
import uniffi.teamcraft_ffi.SearchHitItem

private const val TYPING_PAUSE_MS = 250L

@Composable
fun SearchScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  var query by rememberSaveable { mutableStateOf("") }
  var hits by remember { mutableStateOf<List<SearchHitItem>>(emptyList()) }
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  LaunchedEffect(Unit) { snapshot = viewModel.readChat { snapshot() } }
  LaunchedEffect(query) {
    delay(TYPING_PAUSE_MS)
    hits = viewModel.search(query)
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.search), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      OutlinedTextField(
        value = query,
        onValueChange = { query = it },
        placeholder = { Text(stringResource(R.string.search_hint)) },
        singleLine = true,
        modifier = Modifier.fillMaxWidth().padding(start = 12.dp, end = 12.dp, top = 12.dp),
      )
      SearchFilterBar(snapshot, query) { query = it }
      if (query.isNotBlank() && hits.isEmpty()) {
        Text(stringResource(R.string.search_empty), modifier = Modifier.padding(16.dp))
      }
      LazyColumn {
        items(hits, key = { it.message.id }) { hit ->
          Column(Modifier.fillMaxWidth().clickable { onOpen(hit.message.conversationId) }) {
            Text(
              listOfNotNull(hit.maybeSpaceName, hit.conversationName).joinToString(" › "),
              style = MaterialTheme.typography.labelMedium,
              color = MaterialTheme.colorScheme.primary,
              modifier = Modifier.padding(start = 16.dp, top = 8.dp),
            )
            Text(
              hit.message.authorName,
              style = MaterialTheme.typography.labelLarge,
              modifier = Modifier.padding(start = 16.dp, top = 2.dp),
            )
            Text(hit.message.body, modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 8.dp))
          }
        }
      }
    }
  }
}

/** Which picker the filter bar shows. */
private enum class FilterPicker { From, In, After, Before }

/** Chips that add filter tokens to the query text, and the active ones as removable chips. */
@Composable
private fun SearchFilterBar(maybeSnapshot: ChatSnapshot?, query: String, onQueryChange: (String) -> Unit) {
  var maybePicker by remember { mutableStateOf<FilterPicker?>(null) }
  var isDateMenuOpen by remember { mutableStateOf(false) }
  val addToken = { token: String? -> token?.let { onQueryChange(SearchFilters.add(query, it)) } }
  val toggle = { token: String ->
    onQueryChange(if (SearchFilters.contains(query, token)) SearchFilters.remove(query, token) else SearchFilters.add(query, token))
  }
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
    horizontalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    FilterChip(
      selected = false,
      onClick = { maybePicker = FilterPicker.From },
      label = { Text(stringResource(R.string.search_filter_from)) },
    )
    FilterChip(
      selected = false,
      onClick = { maybePicker = FilterPicker.In },
      label = { Text(stringResource(R.string.search_filter_in)) },
    )
    Box {
      FilterChip(
        selected = false,
        onClick = { isDateMenuOpen = true },
        label = { Text(stringResource(R.string.search_filter_date)) },
      )
      DropdownMenu(expanded = isDateMenuOpen, onDismissRequest = { isDateMenuOpen = false }) {
        DropdownMenuItem(text = { Text(stringResource(R.string.search_filter_after)) }, onClick = {
          isDateMenuOpen = false
          maybePicker = FilterPicker.After
        })
        DropdownMenuItem(text = { Text(stringResource(R.string.search_filter_before)) }, onClick = {
          isDateMenuOpen = false
          maybePicker = FilterPicker.Before
        })
      }
    }
    FilterChip(
      selected = SearchFilters.contains(query, SearchFilters.HAS_FILE),
      onClick = { toggle(SearchFilters.HAS_FILE) },
      label = { Text(stringResource(R.string.search_filter_has_file)) },
    )
    FilterChip(
      selected = SearchFilters.contains(query, SearchFilters.HAS_LINK),
      onClick = { toggle(SearchFilters.HAS_LINK) },
      label = { Text(stringResource(R.string.search_filter_has_link)) },
    )
    FilterChip(
      selected = SearchFilters.contains(query, SearchFilters.IN_THREAD),
      onClick = { toggle(SearchFilters.IN_THREAD) },
      label = { Text(stringResource(R.string.search_filter_in_thread)) },
    )
  }
  ActiveFilters(query, onQueryChange)
  when (maybePicker) {
    FilterPicker.From -> {
      val people = maybeSnapshot?.let { listOf(it.me) + it.users.filter { user -> user.id != it.me.id } }.orEmpty()
      ChoiceDialog(
        title = stringResource(R.string.search_filter_from_title),
        choices = people.sortedBy { it.displayName.lowercase() }.map { it.displayName to it.username },
        onPick = { addToken(SearchFilters.from(it)) },
        onDismiss = { maybePicker = null },
      )
    }
    FilterPicker.In -> {
      val names = maybeSnapshot?.conversations.orEmpty().map { it.name }.filter { it.isNotBlank() }.distinct()
      ChoiceDialog(
        title = stringResource(R.string.search_filter_in_title),
        choices = names.sortedBy { it.lowercase() }.map { it to it },
        onPick = { addToken(SearchFilters.inConversation(it)) },
        onDismiss = { maybePicker = null },
      )
    }
    FilterPicker.After -> DateFilterDialog(
      onPick = { addToken(SearchFilters.after(it)) },
      onDismiss = { maybePicker = null },
    )
    FilterPicker.Before -> DateFilterDialog(
      onPick = { addToken(SearchFilters.before(it)) },
      onDismiss = { maybePicker = null },
    )
    null -> Unit
  }
}

@Composable
private fun ActiveFilters(query: String, onQueryChange: (String) -> Unit) {
  val active = SearchFilters.active(query)
  if (active.isEmpty()) return
  Row(
    Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp),
    horizontalArrangement = Arrangement.spacedBy(8.dp),
  ) {
    active.forEach { token ->
      InputChip(
        selected = true,
        onClick = { onQueryChange(SearchFilters.remove(query, token)) },
        label = { Text(token) },
        trailingIcon = {
          Icon(
            Icons.Filled.Close,
            contentDescription = stringResource(R.string.search_filter_remove),
            modifier = Modifier.size(InputChipDefaults.IconSize),
          )
        },
      )
    }
  }
}

/** A list of `label to value` choices; picking one closes the dialog. */
@Composable
private fun ChoiceDialog(
  title: String,
  choices: List<Pair<String, String>>,
  onPick: (String) -> Unit,
  onDismiss: () -> Unit,
) {
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(title) },
    text = {
      if (choices.isEmpty()) {
        Text(stringResource(R.string.search_filter_no_choices))
      } else {
        LazyColumn(Modifier.heightIn(max = 400.dp)) {
          items(choices) { (label, value) ->
            Text(
              label,
              modifier = Modifier
                .fillMaxWidth()
                .clickable {
                  onPick(value)
                  onDismiss()
                }
                .padding(vertical = 12.dp),
            )
          }
        }
      }
    },
    confirmButton = {},
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun DateFilterDialog(onPick: (LocalDate) -> Unit, onDismiss: () -> Unit) {
  val state = rememberDatePickerState()
  DatePickerDialog(
    onDismissRequest = onDismiss,
    confirmButton = {
      TextButton(
        onClick = {
          state.selectedDateMillis?.let { onPick(SearchFilters.dayOf(it)) }
          onDismiss()
        },
        enabled = state.selectedDateMillis != null,
      ) { Text(stringResource(R.string.ok)) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  ) {
    DatePicker(state = state)
  }
}
