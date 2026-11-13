package de.zeitgleich.teamcraft.calls

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.rememberModalBottomSheetState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.User
import uniffi.teamcraft_ffi.MeetingHostAction
import uniffi.teamcraft_ffi.MeetingHosts
import uniffi.teamcraft_ffi.MeetingPart

/** One person in the call, as the participant list shows them. */
internal class CallPerson(val userId: String, val name: String, val canHost: Boolean)

/** A person's part in the meeting, for the badge. */
internal fun partOf(hosts: MeetingHosts, userId: String): MeetingPart = when (userId) {
  hosts.organizerUserId -> MeetingPart.ORGANIZER
  in hosts.coHostUserIds -> MeetingPart.CO_HOST
  else -> MeetingPart.PARTICIPANT
}

/** What the organizer's menu offers for somebody: the core's three changes, as fit their part. */
internal fun hostActions(hosts: MeetingHosts, person: CallPerson): List<MeetingHostAction> {
  if (hosts.ownPart != MeetingPart.ORGANIZER || !person.canHost) return emptyList()
  return when (partOf(hosts, person.userId)) {
    MeetingPart.ORGANIZER -> emptyList()
    MeetingPart.CO_HOST -> listOf(MeetingHostAction.REMOVE_CO_HOST, MeetingHostAction.HAND_OVER)
    MeetingPart.PARTICIPANT -> listOf(MeetingHostAction.MAKE_CO_HOST, MeetingHostAction.HAND_OVER)
  }
}

/**
 * The people in a meeting's call with their "Host" and "Co-host" badges; the organizer gets a menu
 * per person: make or remove a co-host, hand the host role over (after a confirmation). Below, how
 * the last change went.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun CallPeopleSheet(
  hosts: MeetingHosts,
  people: List<CallPerson>,
  maybeStatus: HostChangeStatus?,
  onChange: (String, MeetingHostAction) -> Unit,
  onDismiss: () -> Unit,
) {
  var maybeHandover by remember { mutableStateOf<CallPerson?>(null) }
  ModalBottomSheet(onDismissRequest = onDismiss, sheetState = rememberModalBottomSheetState()) {
    Column(Modifier.fillMaxWidth().padding(bottom = 24.dp)) {
      Text(
        stringResource(R.string.call_people_title),
        style = MaterialTheme.typography.titleLarge,
        modifier = Modifier.padding(horizontal = 24.dp, vertical = 8.dp),
      )
      HostChangeLine(maybeStatus)
      LazyColumn {
        items(people, key = { it.userId }) { person ->
          PersonRow(
            hosts = hosts,
            person = person,
            onAction = { action ->
              if (action == MeetingHostAction.HAND_OVER) maybeHandover = person else onChange(person.userId, action)
            },
          )
        }
      }
    }
  }
  maybeHandover?.let { person ->
    AlertDialog(
      onDismissRequest = { maybeHandover = null },
      title = { Text(stringResource(R.string.host_hand_over_title, person.name)) },
      text = { Text(stringResource(R.string.host_hand_over_body, person.name)) },
      confirmButton = {
        TextButton(onClick = {
          maybeHandover = null
          onChange(person.userId, MeetingHostAction.HAND_OVER)
        }) { Text(stringResource(R.string.host_hand_over_confirm)) }
      },
      dismissButton = { TextButton(onClick = { maybeHandover = null }) { Text(stringResource(R.string.cancel)) } },
    )
  }
}

@Composable
private fun HostChangeLine(maybeStatus: HostChangeStatus?) {
  val text = when (maybeStatus) {
    null, HostChangeStatus.Saved -> return
    HostChangeStatus.Saving -> stringResource(R.string.host_change_saving)
    is HostChangeStatus.Failed -> stringResource(R.string.host_change_failed, maybeStatus.reason)
  }
  val color = if (maybeStatus is HostChangeStatus.Failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant
  Text(text, color = color, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 24.dp))
}

@Composable
private fun PersonRow(hosts: MeetingHosts, person: CallPerson, onAction: (MeetingHostAction) -> Unit) {
  val actions = hostActions(hosts, person)
  ListItem(
    headlineContent = { Text(person.name) },
    supportingContent = badgeText(partOf(hosts, person.userId))?.let { badge -> { Text(stringResource(badge)) } },
    trailingContent = if (actions.isEmpty()) {
      null
    } else {
      { PersonMenu(person.name, actions, onAction) }
    },
  )
}

@Composable
private fun PersonMenu(name: String, actions: List<MeetingHostAction>, onAction: (MeetingHostAction) -> Unit) {
  var isOpen by remember { mutableStateOf(false) }
  val label = stringResource(R.string.host_menu_for, name)
  Box {
    IconButton(onClick = { isOpen = true }, modifier = Modifier.semantics { contentDescription = label }) {
      Icon(Icons.Filled.MoreVert, contentDescription = null)
    }
    DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
      actions.forEach { action ->
        DropdownMenuItem(
          text = { Text(stringResource(actionText(action))) },
          onClick = {
            isOpen = false
            onAction(action)
          },
        )
      }
    }
  }
}

/**
 * The organizer leaves while people wait in the lobby and nobody else hosts: hand the meeting to
 * somebody in the call and leave, leave anyway, or stay.
 */
@Composable
internal fun LeavePromptDialog(offer: LeaveOffer, onHandOver: (User) -> Unit, onLeave: () -> Unit, onStay: () -> Unit) {
  AlertDialog(
    onDismissRequest = onStay,
    title = { Text(stringResource(R.string.host_leave_title)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(pluralStringResource(R.plurals.host_leave_body, offer.waiting, offer.waiting))
        offer.candidates.forEach { user ->
          OutlinedButton(onClick = { onHandOver(user) }, modifier = Modifier.fillMaxWidth()) {
            Text(stringResource(R.string.host_leave_hand_over, user.displayName))
          }
        }
      }
    },
    confirmButton = { TextButton(onClick = onLeave) { Text(stringResource(R.string.host_leave_anyway)) } },
    dismissButton = { TextButton(onClick = onStay) { Text(stringResource(R.string.cancel)) } },
  )
}

private fun badgeText(part: MeetingPart): Int? = when (part) {
  MeetingPart.ORGANIZER -> R.string.host_role_organizer
  MeetingPart.CO_HOST -> R.string.host_role_co_host
  MeetingPart.PARTICIPANT -> null
}

private fun actionText(action: MeetingHostAction): Int = when (action) {
  MeetingHostAction.MAKE_CO_HOST -> R.string.host_make_co_host
  MeetingHostAction.REMOVE_CO_HOST -> R.string.host_remove_co_host
  MeetingHostAction.HAND_OVER -> R.string.host_hand_over
}
