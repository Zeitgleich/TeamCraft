package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.ui.MainViewModel
import kotlinx.coroutines.launch
import uniffi.teamcraft_ffi.ChannelRenamerRole
import uniffi.teamcraft_ffi.SpaceDeletionChannel
import uniffi.teamcraft_ffi.SpaceDeletionPreview

/** What the confirmation knows of the channels a space's deletion takes. */
private sealed interface SpaceDeletionListing {
  data object Listing : SpaceDeletionListing

  data class Listed(val preview: SpaceDeletionPreview) : SpaceDeletionListing

  /** The core couldn't list them: the confirmation says the count, and the deletion is unsigned. */
  data object Unlisted : SpaceDeletionListing
}

/**
 * The confirmation of a space's deletion (e2ee.md §4.6.5, D7, #1875): lists the channels that go,
 * each with who last named it, as desktop and web do. The core signs exactly the channels listed
 * here, so "Delete" waits for the list. The dialog stays open with the reason when the deletion is
 * refused (forms.md, "Submitting").
 */
@Composable
internal fun SpaceDeleteDialog(viewModel: MainViewModel, spaceId: String, name: String, channelCount: Int, onDismiss: () -> Unit) {
  val scope = rememberCoroutineScope()
  var listing by remember(spaceId) { mutableStateOf<SpaceDeletionListing>(SpaceDeletionListing.Listing) }
  var isDeleting by remember(spaceId) { mutableStateOf(false) }
  var maybeFailure by remember(spaceId) { mutableStateOf<Int?>(null) }
  LaunchedEffect(spaceId) {
    listing = viewModel.prepareSpaceDeletion(spaceId)?.let { SpaceDeletionListing.Listed(it) } ?: SpaceDeletionListing.Unlisted
  }
  val current = listing
  val count = if (current is SpaceDeletionListing.Listed) current.preview.channels.size else channelCount
  AlertDialog(
    onDismissRequest = { if (!isDeleting) onDismiss() },
    title = { Text(stringResource(R.string.space_delete_title, name)) },
    text = {
      Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text(stringResource(R.string.space_delete_text, count))
        when (current) {
          SpaceDeletionListing.Listing -> ListingRow()
          is SpaceDeletionListing.Listed -> if (current.preview.channels.isNotEmpty()) ChannelList(current.preview.channels)
          SpaceDeletionListing.Unlisted -> Unit
        }
        maybeFailure?.let { failure ->
          Text(
            stringResource(failure),
            color = MaterialTheme.colorScheme.error,
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
          )
        }
      }
    },
    confirmButton = {
      TextButton(
        enabled = current !is SpaceDeletionListing.Listing && !isDeleting,
        onClick = {
          isDeleting = true
          maybeFailure = null
          scope.launch {
            val failure = viewModel.tryAct(ChatActions.deleteSpace(spaceId))
            isDeleting = false
            if (failure == null) onDismiss() else maybeFailure = failure
          }
        },
      ) { Text(stringResource(R.string.channel_delete_confirm), color = MaterialTheme.colorScheme.error) }
    },
    dismissButton = { TextButton(onClick = onDismiss, enabled = !isDeleting) { Text(stringResource(R.string.cancel)) } },
  )
}

@Composable
private fun ListingRow() {
  Row(
    verticalAlignment = Alignment.CenterVertically,
    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
  ) {
    PageLoadingIndicator(modifier = Modifier.size(24.dp))
    Spacer(Modifier.width(12.dp))
    Text(stringResource(R.string.space_delete_listing), style = MaterialTheme.typography.bodyMedium)
  }
}

/** Bounded and lazy: a space can hold hundreds of channels. */
@Composable
private fun ChannelList(channels: List<SpaceDeletionChannel>) {
  val label = stringResource(R.string.space_delete_channels)
  LazyColumn(
    verticalArrangement = Arrangement.spacedBy(8.dp),
    modifier = Modifier
      .fillMaxWidth()
      .heightIn(max = 280.dp)
      .semantics { contentDescription = label },
  ) {
    items(channels, key = { it.conversationId }) { channel -> ChannelRow(channel) }
  }
}

@Composable
private fun ChannelRow(channel: SpaceDeletionChannel) {
  Column(modifier = Modifier.fillMaxWidth().padding(vertical = 2.dp)) {
    Text("#${channel.name}", style = MaterialTheme.typography.bodyLarge, color = MaterialTheme.colorScheme.onSurface)
    channel.maybeRenamedBy?.let { renamer ->
      val who = renamer.displayName.ifEmpty { stringResource(R.string.channel_roles_review_unknown) }
      Text(
        stringResource(renamerText(renamer.role), who),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (channel.keepsHistory) {
      Text(
        stringResource(R.string.space_delete_member_name_kept),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
  }
}

@StringRes
private fun renamerText(role: ChannelRenamerRole): Int = when (role) {
  ChannelRenamerRole.FOUNDER -> R.string.space_delete_named_by_founder
  ChannelRenamerRole.CHANNEL_ADMIN, ChannelRenamerRole.WORKSPACE_ADMIN -> R.string.space_delete_renamed_by_admin
  ChannelRenamerRole.MEMBER -> R.string.space_delete_renamed_by_member
}
