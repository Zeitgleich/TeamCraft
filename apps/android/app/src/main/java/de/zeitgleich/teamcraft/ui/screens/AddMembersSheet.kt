package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
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
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.theme.Spacing
import de.zeitgleich.teamcraft.ui.toggled
import kotlinx.coroutines.launch

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
  var picked by remember { mutableStateOf(listOf<String>()) }
  var isAdding by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val members = memberIds.toSet()
  val add: () -> Unit = add@{
    if (isAdding || picked.isEmpty()) return@add
    isAdding = true
    maybeFailure = null
    scope.launch {
      maybeFailure = viewModel.tryAddPeople(conversationId, picked.filter { it !in members })
      isAdding = false
      if (maybeFailure == null) onDismiss()
    }
  }
  ModalBottomSheet(
    onDismissRequest = { if (!isAdding) onDismiss() },
    sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true),
  ) {
    Column(
      Modifier.fillMaxWidth().imePadding().navigationBarsPadding().padding(bottom = Spacing.l),
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      Text(
        stringResource(R.string.members_add),
        style = MaterialTheme.typography.titleLarge,
        modifier = Modifier.padding(horizontal = Spacing.l).semantics { heading() },
      )
      PeoplePicker(
        search = { query -> viewModel.readChat { searchPeople(query, PEOPLE_SHOWN) } },
        changes = changes,
        picked = picked,
        onPick = { user -> picked = toggled(picked, user.id) },
        excluded = members,
        isEnabled = !isAdding,
        modifier = Modifier.heightIn(max = 480.dp),
      )
      // Why adding stopped, next to the button (forms.md).
      maybeFailure?.let { failure ->
        Text(
          stringResource(failure),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodyMedium,
          modifier = Modifier.padding(horizontal = Spacing.l).semantics { liveRegion = LiveRegionMode.Polite },
        )
      }
      Button(
        onClick = add,
        enabled = picked.isNotEmpty() && !isAdding,
        modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l),
      ) {
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
