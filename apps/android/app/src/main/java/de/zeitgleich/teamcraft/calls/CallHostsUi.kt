package de.zeitgleich.teamcraft.calls

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.MeetingHosts

private val HOSTS_LIST_HEIGHT = 360.dp

/**
 * The organizer's host menu in a meeting's call (#1720), as desktop's host controls have it:
 * everybody else in the call with their part ("Host", "Co-host"), "Make co-host" or "Remove as
 * co-host", and "Hand over host role" after a confirmation (the organizer stays co-host).
 */
@Composable
internal fun HostsDialog(
  hosts: MeetingHosts,
  people: List<User>,
  change: HostsChange,
  onAction: (User, MeetingHostAction) -> Unit,
  onDismissFailure: () -> Unit,
  onClose: () -> Unit,
) {
  var maybeHandingTo by remember { mutableStateOf<User?>(null) }
  val isSaving = change == HostsChange.Saving
  AlertDialog(
    onDismissRequest = onClose,
    title = { Text(stringResource(R.string.call_hosts_title)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(stringResource(R.string.call_hosts_hint), style = MaterialTheme.typography.bodySmall)
        LazyColumn(Modifier.heightIn(max = HOSTS_LIST_HEIGHT), verticalArrangement = Arrangement.spacedBy(12.dp)) {
          items(people, key = { it.id }) { user ->
            HostRow(hosts, user, isEnabled = !isSaving) { action ->
              if (action == MeetingHostAction.HAND_OVER) maybeHandingTo = user else onAction(user, action)
            }
          }
        }
        when (change) {
          HostsChange.Saving -> Row(
            Modifier.semantics { liveRegion = LiveRegionMode.Polite },
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            verticalAlignment = Alignment.CenterVertically,
          ) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            Text(stringResource(R.string.call_hosts_saving), style = MaterialTheme.typography.bodySmall)
          }
          is HostsChange.Failed -> Text(
            stringResource(R.string.call_hosts_failed, change.maybeReason ?: stringResource(change.fallback)),
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Assertive },
          )
          HostsChange.Idle -> Unit
        }
      }
    },
    confirmButton = {
      TextButton(onClick = {
        onDismissFailure()
        onClose()
      }) { Text(stringResource(R.string.close)) }
    },
  )
  maybeHandingTo?.let { user ->
    AlertDialog(
      onDismissRequest = { maybeHandingTo = null },
      title = { Text(stringResource(R.string.call_hosts_hand_over)) },
      text = { Text(stringResource(R.string.call_hosts_hand_over_confirm, user.displayName)) },
      confirmButton = {
        Button(onClick = {
          onAction(user, MeetingHostAction.HAND_OVER)
          maybeHandingTo = null
        }) { Text(stringResource(R.string.call_hosts_hand_over_do)) }
      },
      dismissButton = { TextButton(onClick = { maybeHandingTo = null }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun HostRow(hosts: MeetingHosts, user: User, isEnabled: Boolean, onAction: (MeetingHostAction) -> Unit) {
  Column(Modifier.fillMaxWidth(), verticalArrangement = Arrangement.spacedBy(4.dp)) {
    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
      Text(
        user.displayName,
        style = MaterialTheme.typography.bodyLarge,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.weight(1f, fill = false),
      )
      hostLabelOf(hosts, user.id)?.let { label ->
        Surface(color = MaterialTheme.colorScheme.secondaryContainer, shape = MaterialTheme.shapes.small) {
          Text(
            stringResource(label),
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.padding(horizontal = 8.dp, vertical = 2.dp),
          )
        }
      }
    }
    val actions = hostActionsFor(hosts, user)
    if (actions.isNotEmpty()) {
      FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        actions.forEach { action ->
          OutlinedButton(onClick = { onAction(action) }, enabled = isEnabled) {
            Text(stringResource(hostActionText(action)))
          }
        }
      }
    }
  }
}

private fun hostActionText(action: MeetingHostAction): Int = when (action) {
  MeetingHostAction.MAKE_CO_HOST -> R.string.call_hosts_make_co_host
  MeetingHostAction.REMOVE_CO_HOST -> R.string.call_hosts_remove_co_host
  MeetingHostAction.HAND_OVER -> R.string.call_hosts_hand_over
}

/**
 * The organizer leaves while people wait in the lobby and nobody else can let them in (#1720), as
 * desktop and web ask: hand the meeting to one of the others here and leave, leave anyway, or stay.
 */
@Composable
internal fun LeaveHandoverDialog(
  offer: HandoverOffer,
  onHandOver: (User) -> Unit,
  onLeave: () -> Unit,
  onStay: () -> Unit,
) {
  AlertDialog(
    onDismissRequest = onStay,
    title = { Text(stringResource(R.string.call_hosts_leave_title)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(pluralStringResource(R.plurals.call_hosts_leave_body, offer.waiting, offer.waiting))
        offer.candidates.forEach { user ->
          Button(onClick = { onHandOver(user) }, modifier = Modifier.fillMaxWidth()) {
            Text(
              stringResource(R.string.call_hosts_leave_hand_over, user.displayName),
              maxLines = 1,
              overflow = TextOverflow.Ellipsis,
            )
          }
        }
        OutlinedButton(onClick = onLeave, modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) {
          Text(stringResource(R.string.call_hosts_leave_anyway))
        }
      }
    },
    confirmButton = { TextButton(onClick = onStay) { Text(stringResource(R.string.cancel)) } },
  )
}
