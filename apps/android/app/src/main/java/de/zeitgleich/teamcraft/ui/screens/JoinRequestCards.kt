package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.JoinRequestActions
import de.zeitgleich.teamcraft.ui.theme.Spacing
import uniffi.teamcraft_ffi.JoinHistory
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.JoinRequestStanding

/**
 * Admins' requests to join channels the user owns, at the top of Activity (admin.md §2.4.1): each
 * with what approving shares, or why only Decline is offered.
 */
@Composable
fun JoinRequestSection(requests: List<JoinRequestItem>, actions: JoinRequestActions) {
  val pending by actions.pending.collectAsState()
  val title = stringResource(R.string.join_requests_title)
  Column(Modifier.fillMaxWidth().padding(bottom = Spacing.s)) {
    Text(
      title.uppercase(),
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier
        .padding(start = Spacing.l, top = Spacing.l, bottom = Spacing.xs)
        .semantics {
          heading()
          contentDescription = title
        },
    )
    for (request in requests) {
      JoinRequestCard(
        request = request,
        isPending = JoinRequestActions.key(request) in pending,
        isInChannel = false,
        onApprove = { history -> actions.approve(request, history) },
        onDecline = { actions.decline(request) },
      )
    }
  }
}

/** The requests for the open channel, above its messages: the owner decides where they are. */
@Composable
fun JoinRequestBanner(requests: List<JoinRequestItem>, actions: JoinRequestActions) {
  if (requests.isEmpty()) return
  val pending by actions.pending.collectAsState()
  Column(Modifier.fillMaxWidth().padding(vertical = Spacing.xs)) {
    for (request in requests) {
      JoinRequestCard(
        request = request,
        isPending = JoinRequestActions.key(request) in pending,
        isInChannel = true,
        onApprove = { history -> actions.approve(request, history) },
        onDecline = { actions.decline(request) },
      )
    }
  }
}

/**
 * One request, naming the admin with their username (two people may share a name). Approve is the
 * primary action and only there for an admin of the fresh verified roster; it shares what a
 * self-join gets, the history from the join on, and the earlier history only when the owner ticks
 * it. Decline is plain (the admin can ask again later). While a decision runs, everything waits.
 * [isInChannel]: shown in the channel itself, which needs no channel name.
 */
@Composable
private fun JoinRequestCard(
  request: JoinRequestItem,
  isPending: Boolean,
  isInChannel: Boolean,
  onApprove: (JoinHistory) -> Unit,
  onDecline: () -> Unit,
) {
  val isApprovable = request.standing == JoinRequestStanding.APPROVABLE
  var sharesHistory by rememberSaveable(request.notificationTag) { mutableStateOf(false) }
  val summary = when {
    isInChannel || request.conversationName.isEmpty() -> stringResource(R.string.join_request_summary_here, request.userLabel)
    else -> stringResource(R.string.join_request_summary, request.userLabel, "#" + request.conversationName)
  }
  val note = stringResource(
    when {
      !isApprovable -> R.string.join_request_not_admin
      sharesHistory -> R.string.join_request_hint_history
      else -> R.string.join_request_hint
    },
  )
  Surface(
    color = MaterialTheme.colorScheme.surfaceContainer,
    shape = MaterialTheme.shapes.large,
    modifier = Modifier.fillMaxWidth().padding(horizontal = Spacing.l, vertical = Spacing.xs),
  ) {
    Column(Modifier.padding(Spacing.l), verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
      Text(summary, style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurface)
      Text(note, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
      if (isApprovable) {
        Row(
          Modifier
            .fillMaxWidth()
            .toggleable(value = sharesHistory, enabled = !isPending, role = Role.Checkbox) { sharesHistory = it },
          verticalAlignment = Alignment.CenterVertically,
        ) {
          Checkbox(checked = sharesHistory, onCheckedChange = null, enabled = !isPending)
          Text(
            stringResource(R.string.join_request_share_history),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(start = Spacing.s),
          )
        }
      }
      Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(Spacing.s, Alignment.End),
        verticalAlignment = Alignment.CenterVertically,
      ) {
        if (isPending) {
          CircularProgressIndicator(Modifier.size(18.dp), strokeWidth = 2.dp)
        }
        OutlinedButton(onClick = onDecline, enabled = !isPending) {
          Text(stringResource(R.string.join_request_decline))
        }
        if (isApprovable) {
          Button(
            onClick = { onApprove(if (sharesHistory) JoinHistory.FULL else JoinHistory.FROM_JOIN) },
            enabled = !isPending,
          ) {
            Text(stringResource(R.string.join_request_approve))
          }
        }
      }
    }
  }
}
