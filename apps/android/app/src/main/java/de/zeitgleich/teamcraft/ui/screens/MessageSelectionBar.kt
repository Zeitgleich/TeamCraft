package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R

/**
 * The top bar of selection mode (issue #2070): the count, Cancel, and forward, copy and delete.
 * Delete is off, with its reason under the count, when a selected message isn't the user's own:
 * the hint comes before the click, never an error after it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun MessageSelectionBar(
  count: Int,
  canDelete: Boolean,
  onCancel: () -> Unit,
  onForward: () -> Unit,
  onCopy: () -> Unit,
  onDelete: () -> Unit,
) {
  var isConfirmingDelete by remember { mutableStateOf(false) }
  val isEmpty = count == 0
  TopAppBar(
    navigationIcon = {
      IconButton(onClick = onCancel) {
        Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.cancel))
      }
    },
    title = {
      Column {
        Text(
          pluralStringResource(R.plurals.selection_count, count, count),
          style = MaterialTheme.typography.titleMedium,
          modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
        )
        if (!canDelete && !isEmpty) {
          Text(
            stringResource(R.string.selection_delete_refused),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.padding(top = 2.dp),
          )
        }
      }
    },
    actions = {
      IconButton(onClick = onForward, enabled = !isEmpty) {
        Icon(painterResource(R.drawable.ic_forward), contentDescription = stringResource(R.string.action_forward))
      }
      IconButton(onClick = onCopy, enabled = !isEmpty) {
        Icon(painterResource(R.drawable.ic_content_copy), contentDescription = stringResource(R.string.selection_copy))
      }
      IconButton(onClick = { isConfirmingDelete = true }, enabled = canDelete) {
        Icon(Icons.Filled.Delete, contentDescription = stringResource(R.string.action_delete))
      }
    },
  )
  if (isConfirmingDelete && canDelete) {
    AlertDialog(
      onDismissRequest = { isConfirmingDelete = false },
      title = { Text(pluralStringResource(R.plurals.selection_delete_confirm, count, count)) },
      text = { Text(stringResource(R.string.delete_confirm_text)) },
      confirmButton = {
        TextButton(onClick = {
          isConfirmingDelete = false
          onDelete()
        }) { Text(stringResource(R.string.action_delete), color = MaterialTheme.colorScheme.error) }
      },
      dismissButton = { TextButton(onClick = { isConfirmingDelete = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}
