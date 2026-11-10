package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemShapes
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.PresenceDot
import de.zeitgleich.teamcraft.ui.PresenceStyle
import de.zeitgleich.teamcraft.ui.ResetAfter
import de.zeitgleich.teamcraft.ui.presencePickLabel
import java.time.Instant
import java.time.ZoneId
import uniffi.teamcraft_ffi.Presence

/**
 * The status row of the account page and its sheet (#1578): the Teams-style list in the order of
 * the desktop and web app, the current status checked, a "Reset after" for the next pick, and
 * "Reset status" back to automatic. Available is automatic and takes no end.
 */
@Composable
internal fun StatusRow(shapes: ListItemShapes, presence: Presence, isManual: Boolean, onChoose: (Presence, Instant?) -> Unit) {
  var isOpen by rememberSaveable { mutableStateOf(false) }
  SettingsLinkRow(
    shapes,
    stringResource(R.string.presence_title),
    stringResource(presencePickLabel(presence)),
    leadingContent = { PresenceDot(presence, size = 12.dp) },
    onClick = { isOpen = true },
  )
  if (isOpen) {
    StatusSheet(presence, isManual, onDismiss = { isOpen = false }) { choice, maybeUntil ->
      isOpen = false
      onChoose(choice, maybeUntil)
    }
  }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun StatusSheet(
  presence: Presence,
  isManual: Boolean,
  onDismiss: () -> Unit,
  onChoose: (Presence, Instant?) -> Unit,
) {
  val context = LocalContext.current
  var resetAfter by rememberSaveable { mutableStateOf(ResetAfter.NEVER) }
  var hasPastTime by rememberSaveable { mutableStateOf(false) }
  fun choose(choice: Presence) {
    hasPastTime = false
    when {
      choice == Presence.ONLINE -> onChoose(choice, null)
      resetAfter == ResetAfter.UNTIL_TIME -> pickDateTime(context, onPast = { hasPastTime = true }) { onChoose(choice, it) }
      else -> onChoose(choice, resetAfter.end(Instant.now(), ZoneId.systemDefault()))
    }
  }
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState(skipPartiallyExpanded = true)) {
    Column(Modifier.padding(bottom = 24.dp)) {
      Text(
        stringResource(R.string.status_duration_reset_after),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.padding(horizontal = 16.dp),
      )
      FlowRow(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.padding(horizontal = 16.dp).selectableGroup(),
      ) {
        for (choice in ResetAfter.entries) {
          FilterChip(selected = resetAfter == choice, onClick = { resetAfter = choice }, label = { Text(stringResource(choice.label)) })
        }
      }
      if (hasPastTime) {
        Text(
          stringResource(R.string.status_duration_until_past),
          color = MaterialTheme.colorScheme.error,
          style = MaterialTheme.typography.bodySmall,
          modifier = Modifier.padding(horizontal = 16.dp),
        )
      }
      Column(Modifier.selectableGroup()) {
        for (choice in PresenceStyle.ALL) {
          val isCurrent = choice == presence
          ListItem(
            headlineContent = { Text(stringResource(presencePickLabel(choice))) },
            leadingContent = { Box(Modifier.padding(4.dp)) { PresenceDot(choice, size = 12.dp) } },
            trailingContent = { if (isCurrent) Icon(Icons.Filled.Check, contentDescription = null) },
            modifier = Modifier.selectable(selected = isCurrent, role = Role.RadioButton, onClick = { choose(choice) }),
          )
        }
      }
      if (isManual) {
        ListItem(
          headlineContent = { Text(stringResource(R.string.status_reset)) },
          leadingContent = { Icon(Icons.Filled.Refresh, contentDescription = null) },
          modifier = Modifier.clickable { onChoose(Presence.ONLINE, null) },
        )
      }
    }
  }
}
