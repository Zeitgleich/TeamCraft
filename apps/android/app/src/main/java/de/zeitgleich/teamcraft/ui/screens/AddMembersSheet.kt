package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import de.zeitgleich.teamcraft.ui.toggled
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/** How long the search waits after a keystroke before it asks the directory. */
private const val ADD_SEARCH_PAUSE_MS = 250L

/** The most people the search lists; typing narrows it. */
private const val ADD_SHOWN = 100

/**
 * "Add people" to a channel (#1534), where the server says the user may: the directory through the
 * core, without the members, several at once. As on desktop, people join the channel's space first
 * where the user may add them there. The sheet closes once everybody is in; on a failure it stays
 * with the picks and says why (members added meanwhile drop out of the list).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun AddMembersSheet(viewModel: MainViewModel, conversationId: String, memberIds: List<String>, onDismiss: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  val scope = rememberCoroutineScope()
  var query by remember { mutableStateOf("") }
  var maybeFound by remember { mutableStateOf<List<User>?>(null) }
  var picked by remember { mutableStateOf(listOf<String>()) }
  var pickedPeople by remember { mutableStateOf(mapOf<String, User>()) }
  var isAdding by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  LaunchedEffect(changes, query) {
    if (query.isNotEmpty()) delay(ADD_SEARCH_PAUSE_MS)
    viewModel.readChat { searchPeople(query.trim(), ADD_SHOWN) }?.let { found -> maybeFound = found }
  }
  val members = memberIds.toSet()
  val found = maybeFound.orEmpty().filter { it.id !in members }
  val shown = picked.mapNotNull { id -> found.firstOrNull { it.id == id } ?: pickedPeople[id] } + found.filter { it.id !in picked }
  val add: () -> Unit = add@{
    if (isAdding || picked.isEmpty()) return@add
    isAdding = true
    maybeFailure = null
    scope.launch {
      maybeFailure = viewModel.tryAddPeople(conversationId, picked)
      isAdding = false
      if (maybeFailure == null) onDismiss()
    }
  }
  ModalBottomSheet(
    onDismissRequest = { if (!isAdding) onDismiss() },
    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
  ) {
    Column(
      Modifier.fillMaxWidth().imePadding().navigationBarsPadding().padding(horizontal = Spacing.l).padding(bottom = Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      Text(stringResource(R.string.members_add), style = MaterialTheme.typography.titleLarge, modifier = Modifier.semantics { heading() })
      CompactSearchField(value = query, onChange = { query = it }, placeholder = stringResource(R.string.browse_filter_people))
      LazyColumn(Modifier.fillMaxWidth().heightIn(max = 420.dp)) {
        if (maybeFound != null && shown.isEmpty()) {
          item { Text(stringResource(R.string.members_add_none), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(Spacing.l)) }
        }
        items(shown, key = { it.id }) { user ->
          val isPicked = user.id in picked
          ListItem(
            headlineContent = { Text(user.displayName, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            supportingContent = { Text("@" + user.username, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            trailingContent = { Checkbox(checked = isPicked, onCheckedChange = null) },
            colors = ListItemDefaults.colors(containerColor = MaterialTheme.colorScheme.surfaceContainerLow),
            modifier = Modifier.toggleable(value = isPicked, role = Role.Checkbox, enabled = !isAdding) {
              picked = toggled(picked, user.id)
              pickedPeople = pickedPeople + (user.id to user)
            },
          )
        }
      }
      // Why adding stopped, next to the button (forms.md); those still picked weren't added.
      maybeFailure?.let { failure ->
        Text(
          stringResource(failure),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Button(onClick = add, enabled = picked.isNotEmpty() && !isAdding, modifier = Modifier.fillMaxWidth()) {
        if (isAdding) {
          CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
        } else {
          Text(
            if (picked.isEmpty()) {
              stringResource(R.string.members_add_submit)
            } else {
              pluralStringResource(R.plurals.members_add_count, picked.size, picked.size)
            },
          )
        }
      }
    }
  }
}
