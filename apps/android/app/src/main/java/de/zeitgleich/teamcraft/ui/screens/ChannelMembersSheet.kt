package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.channelRoleAppointedByText
import de.zeitgleich.teamcraft.ui.channelRoleLabel
import de.zeitgleich.teamcraft.ui.droppedRoleNames
import de.zeitgleich.teamcraft.ui.reviewedRolesMessage
import de.zeitgleich.teamcraft.ui.roleKeyMarkLabel
import de.zeitgleich.teamcraft.ui.roleReviewRefusalText
import de.zeitgleich.teamcraft.ui.theme.Spacing
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.ChannelMemberItem
import uniffi.teamcraft_ffi.ChannelMembersInfo
import uniffi.teamcraft_ffi.ChannelRolesReviewItem
import uniffi.teamcraft_ffi.RoleKeyMarkItem
import uniffi.teamcraft_ffi.RoleReviewRowItem
import uniffi.teamcraft_ffi.channelRolesReviewHasChanged
import uniffi.teamcraft_ffi.channelRolesReviewHasNew

/**
 * A channel's members (issue #1262), as the members section on desktop and web: everybody with
 * their role, the line for a channel where this device and a member's see different roles, and for
 * a workspace admin the look at the roles (e2ee.md §4.6.1). That look asks nothing of them: it is
 * here when they open the members. What they confirm is the list as it stood when they opened it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ChannelMembersSheet(viewModel: MainViewModel, conversationId: String, onDismiss: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var maybeInfo by remember(conversationId) { mutableStateOf<ChannelMembersInfo?>(null) }
  // The members and the signed roles are read afresh while the section is open.
  LaunchedEffect(conversationId) { viewModel.refreshChannelMembers(conversationId) }
  LaunchedEffect(conversationId, changes) { maybeInfo = viewModel.channelMembers(conversationId) }
  // Held here, not in the list's item: scrolling the review away doesn't close it.
  val shownReview = remember(conversationId) { mutableStateOf<ChannelRolesReviewItem?>(null) }
  // "Add people", where the server says the user may (#1534).
  var canAdd by remember(conversationId) { mutableStateOf(false) }
  var isAdding by remember(conversationId) { mutableStateOf(false) }
  LaunchedEffect(conversationId, changes) {
    canAdd = viewModel.readChat { snapshot() }?.conversations?.firstOrNull { it.id == conversationId }?.canAddMembers == true
  }
  if (isAdding) {
    AddMembersSheet(
      viewModel = viewModel,
      conversationId = conversationId,
      memberIds = maybeInfo?.members?.map { it.userId }.orEmpty(),
      onDismiss = { isAdding = false },
    )
    return
  }
  ModalBottomSheet(onDismissRequest = onDismiss) {
    val info = maybeInfo
    LazyColumn(
      Modifier.fillMaxWidth().padding(horizontal = Spacing.l).padding(bottom = Spacing.xxl),
      verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
      item {
        Text(
          stringResource(R.string.members_title),
          style = MaterialTheme.typography.titleMedium,
          modifier = Modifier.semantics { heading() },
        )
      }
      if (canAdd) {
        item {
          OutlinedButton(onClick = { isAdding = true }, modifier = Modifier.fillMaxWidth()) {
            Text(stringResource(R.string.members_add))
          }
        }
      }
      if (info == null) {
        item { CircularProgressIndicator(Modifier.padding(Spacing.l).size(24.dp)) }
        return@LazyColumn
      }
      if (info.hasRoleAlarm) {
        item { Hint(stringResource(R.string.channel_roles_alarm), isStatus = true) }
      }
      info.maybeReview?.let { review ->
        item { RolesReview(viewModel, conversationId, review, shownReview) }
      }
      items(info.members, key = { it.userId }) { member -> MemberRow(member) }
    }
  }
}

@Composable
private fun MemberRow(member: ChannelMemberItem) {
  Column(Modifier.fillMaxWidth().padding(vertical = Spacing.xs)) {
    Row(verticalAlignment = Alignment.CenterVertically) {
      Text(
        member.name,
        style = MaterialTheme.typography.bodyLarge,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        modifier = Modifier.weight(1f),
      )
      channelRoleLabel(member.role)?.let { Badge(stringResource(it)) }
    }
    // Only where the verified role log says who appointed the role (issue #1246).
    member.maybeAppointer?.let { appointer ->
      Hint(stringResource(channelRoleAppointedByText(appointer), *listOfNotNull(appointer.maybeName).toTypedArray()))
    }
  }
}

/** A workspace admin's look at the roles, as `ChannelRolesReviewPanel` on desktop and web. */
@Composable
private fun RolesReview(
  viewModel: MainViewModel,
  conversationId: String,
  live: ChannelRolesReviewItem,
  // The list and the log as they stood when the admin opened it: that's what they confirm.
  shownReview: MutableState<ChannelRolesReviewItem?>,
) {
  val scope = rememberCoroutineScope()
  var maybeShown by shownReview
  var isSigning by remember { mutableStateOf(false) }
  var maybeMessage by remember { mutableStateOf<Int?>(null) }
  Surface(color = MaterialTheme.colorScheme.surfaceContainerHigh, shape = MaterialTheme.shapes.medium) {
    Column(Modifier.fillMaxWidth().padding(Spacing.m), verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
      Text(
        stringResource(R.string.channel_roles_review_title),
        style = MaterialTheme.typography.titleSmall,
        modifier = Modifier.semantics { heading() },
      )
      Hint(stringResource(if (live.needsALook) R.string.channel_roles_review_hint else R.string.channel_roles_review_hint_quiet))
      val shown = maybeShown
      if (shown == null) {
        OutlinedButton(onClick = {
          maybeMessage = null
          maybeShown = live
        }) { Text(stringResource(R.string.channel_roles_review_action)) }
        maybeMessage?.let { Hint(stringResource(it), isStatus = true) }
        return@Column
      }
      for (row in shown.rows) ReviewRow(row)
      if (channelRolesReviewHasNew(shown)) Hint(stringResource(R.string.channel_roles_review_new_hint))
      if (shown.knowsNoRoles) Hint(stringResource(R.string.channel_roles_review_knows_none))
      if (shown.rows.any { it.keyMark == RoleKeyMarkItem.CHANGED }) {
        Hint(stringResource(R.string.channel_roles_review_key_changed_hint))
      }
      if (shown.dropped.isNotEmpty()) {
        val names = droppedRoleNames(shown.dropped, stringResource(R.string.channel_roles_review_unknown))
        Hint(stringResource(R.string.channel_roles_review_dropped, names))
      }
      shown.maybeRefusal?.let { Hint(stringResource(roleReviewRefusalText(it)), isStatus = true) }
      val hasChanged = channelRolesReviewHasChanged(shown, live)
      if (hasChanged) Hint(stringResource(R.string.channel_roles_review_changed), isStatus = true)
      maybeMessage?.let { Hint(stringResource(it), isStatus = true) }
      Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s), verticalAlignment = Alignment.CenterVertically) {
        Button(
          enabled = !isSigning && shown.maybeRefusal == null && !hasChanged,
          onClick = {
            isSigning = true
            scope.launch {
              val maybeOutcome = viewModel.signReviewedChannelRoles(conversationId, shown)
              isSigning = false
              maybeMessage = maybeOutcome?.let(::reviewedRolesMessage)
              // Done, or nothing signed: the next look starts from the list as it is now.
              if (maybeOutcome != null) maybeShown = null
            }
          },
        ) { Text(stringResource(R.string.channel_roles_review_confirm)) }
        if (hasChanged) {
          OutlinedButton(onClick = {
            maybeMessage = null
            maybeShown = live
          }) { Text(stringResource(R.string.channel_roles_review_action)) }
        }
        TextButton(onClick = { maybeShown = null }) { Text(stringResource(R.string.cancel)) }
      }
    }
  }
}

@Composable
private fun ReviewRow(row: RoleReviewRowItem) {
  Row(
    Modifier.fillMaxWidth(),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
  ) {
    Text(
      row.maybeName ?: stringResource(R.string.channel_roles_review_unknown),
      style = MaterialTheme.typography.bodyMedium,
      color = if (row.maybeName == null) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.weight(1f),
    )
    if (row.isNew) Badge(stringResource(R.string.channel_roles_review_new), isWarning = true)
    roleKeyMarkLabel(row.keyMark)?.let { Badge(stringResource(it), isWarning = true) }
    channelRoleLabel(row.role)?.let { Badge(stringResource(it)) }
  }
}

@Composable
private fun Badge(text: String, isWarning: Boolean = false) {
  val scheme = MaterialTheme.colorScheme
  Surface(
    color = if (isWarning) scheme.tertiaryContainer else scheme.secondaryContainer,
    contentColor = if (isWarning) scheme.onTertiaryContainer else scheme.onSecondaryContainer,
    shape = MaterialTheme.shapes.small,
  ) {
    Text(text, style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 6.dp, vertical = 2.dp))
  }
}

@Composable
private fun Hint(text: String, isStatus: Boolean = false) {
  Text(
    text,
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = if (isStatus) Modifier.semantics { liveRegion = LiveRegionMode.Polite } else Modifier,
  )
}
