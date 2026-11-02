package de.zeitgleich.teamcraft.ui.screens

import android.content.ClipData
import android.content.ClipboardManager
import androidx.compose.foundation.layout.Column
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Done
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Share
import androidx.compose.material.icons.filled.Check
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.ui.EntryAction
import de.zeitgleich.teamcraft.ui.EntryItem
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.conversationMenu
import de.zeitgleich.teamcraft.ui.spaceMenu
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.LeaveChoice

/**
 * The long-press menu of a conversation row (#1105). "Select" is first and starts the multi-selection;
 * the rest are the actions of the desktop's entry menu. What it offers is decided by the core
 * ([LeaveChoice]) and [conversationMenu]; this only draws it and runs the choices.
 */
@Composable
fun ConversationEntryMenu(
  viewModel: MainViewModel,
  conversation: ConversationItem,
  isOpen: Boolean,
  onDismiss: () -> Unit,
  onSelect: () -> Unit,
  onNotifications: () -> Unit,
) {
  val context = LocalContext.current
  var leave by remember(conversation.id) { mutableStateOf(LeaveChoice.HIDDEN) }
  var maybeLink by remember(conversation.id) { mutableStateOf<String?>(null) }
  var isConfirmingLeave by remember(conversation.id) { mutableStateOf(false) }
  var isConfirmingDelete by remember(conversation.id) { mutableStateOf(false) }
  // Read when the menu opens, and again after the core has read the roles it was missing.
  LaunchedEffect(isOpen, conversation.id) {
    if (!isOpen) return@LaunchedEffect
    leave = viewModel.leaveChoice(conversation.id)
    maybeLink = viewModel.conversationLink(conversation.id)
  }
  DropdownMenu(expanded = isOpen, onDismissRequest = onDismiss) {
    for (entry in conversationMenu(conversation, leave, hasLink = maybeLink != null)) {
      val lastAdminHint = if (conversation.canDeleteChannel) {
        R.string.entry_leave_last_channel_admin_or_delete
      } else {
        R.string.entry_leave_last_channel_admin
      }
      EntryMenuItem(entry, lastAdminHint = lastAdminHint) {
        onDismiss()
        when (entry.action) {
          EntryAction.Select -> onSelect()
          EntryAction.MarkRead -> viewModel.markRead(conversation.id)
          EntryAction.Notifications -> onNotifications()
          EntryAction.CopyLink -> maybeLink?.let { copyLink(context, it) }
          EntryAction.LeaveChannel -> isConfirmingLeave = true
          EntryAction.DeleteChannel -> isConfirmingDelete = true
          else -> Unit
        }
      }
    }
  }
  if (isConfirmingLeave) {
    ConfirmDialog(
      title = stringResource(R.string.channel_leave_title, conversation.name),
      text = stringResource(leaveText(leave)),
      confirmLabel = stringResource(R.string.channel_leave_confirm),
      onConfirm = { viewModel.act(ChatActions.leaveChannel(conversation.id, leave == LeaveChoice.LAST_MEMBER)) },
      onDismiss = { isConfirmingLeave = false },
    )
  }
  if (isConfirmingDelete) {
    ConfirmDialog(
      title = stringResource(R.string.channel_delete_title, conversation.name),
      text = stringResource(R.string.channel_delete_text),
      confirmLabel = stringResource(R.string.channel_delete_confirm),
      onConfirm = { viewModel.act(ChatActions.deleteChannel(conversation.id)) },
      onDismiss = { isConfirmingDelete = false },
    )
  }
}

/** The long-press menu of a space's header: mute (its channels follow), mark all read, leave. */
@Composable
fun SpaceEntryMenu(viewModel: MainViewModel, spaceId: String, name: String, isOpen: Boolean, onDismiss: () -> Unit) {
  var maybeMenu by remember(spaceId) { mutableStateOf<uniffi.teamcraft_ffi.SpaceMenuItem?>(null) }
  var isMuting by remember(spaceId) { mutableStateOf(false) }
  var isConfirmingLeave by remember(spaceId) { mutableStateOf(false) }
  var isConfirmingDelete by remember(spaceId) { mutableStateOf(false) }
  LaunchedEffect(isOpen, spaceId) {
    if (isOpen) maybeMenu = viewModel.spaceMenu(spaceId)
  }
  val menu = maybeMenu
  DropdownMenu(expanded = isOpen && menu != null, onDismissRequest = onDismiss) {
    if (menu != null) {
      for (entry in spaceMenu(menu.hasUnread, menu.leave, menu.canDelete)) {
        EntryMenuItem(entry, lastAdminHint = R.string.entry_leave_last_space_admin) {
          onDismiss()
          when (entry.action) {
            EntryAction.MuteSpace -> isMuting = true
            EntryAction.MarkSpaceRead -> viewModel.markSpaceRead(spaceId)
            EntryAction.LeaveSpace -> isConfirmingLeave = true
            EntryAction.DeleteSpace -> isConfirmingDelete = true
            else -> Unit
          }
        }
      }
    }
  }
  if (isMuting) SpaceMuteDialog(viewModel, spaceId, name) { isMuting = false }
  if (isConfirmingDelete) {
    ConfirmDialog(
      title = stringResource(R.string.space_delete_title, name),
      text = stringResource(R.string.space_delete_text, maybeMenu?.channelCount?.toInt() ?: 0),
      confirmLabel = stringResource(R.string.channel_delete_confirm),
      onConfirm = { viewModel.act(ChatActions.deleteSpace(spaceId)) },
      onDismiss = { isConfirmingDelete = false },
    )
  }
  if (isConfirmingLeave) {
    ConfirmDialog(
      title = stringResource(R.string.space_leave_title, name),
      text = stringResource(R.string.space_leave_text),
      confirmLabel = stringResource(R.string.space_leave_confirm),
      onConfirm = { viewModel.leaveSpace(spaceId) },
      onDismiss = { isConfirmingLeave = false },
    )
  }
}

@Composable
private fun EntryMenuItem(entry: EntryItem, lastAdminHint: Int, onClick: () -> Unit) {
  val (label, icon) = look(entry.action)
  DropdownMenuItem(
    text = {
      Column {
        Text(stringResource(label), color = if (isDestructive(entry.action)) MaterialTheme.colorScheme.error else Color.Unspecified)
        // A refusal the user can fix is said before the click, not after (#691).
        if (!entry.isEnabled) Text(stringResource(lastAdminHint), style = MaterialTheme.typography.bodySmall)
      }
    },
    leadingIcon = { MenuIcon(icon) },
    enabled = entry.isEnabled,
    onClick = onClick,
  )
}

/** A menu icon: a vector, or a drawable where material-icons-core has none. */
private sealed interface MenuGlyph {
  data class Vector(val image: ImageVector) : MenuGlyph
  data class Drawable(val id: Int) : MenuGlyph
}

@Composable
private fun MenuIcon(glyph: MenuGlyph) = when (glyph) {
  is MenuGlyph.Vector -> Icon(glyph.image, contentDescription = null)
  is MenuGlyph.Drawable -> Icon(painterResource(glyph.id), contentDescription = null)
}

private fun look(action: EntryAction): Pair<Int, MenuGlyph> = when (action) {
  EntryAction.Select -> R.string.entry_select to MenuGlyph.Vector(Icons.Filled.Check)
  EntryAction.MarkRead -> R.string.chat_mark_read to MenuGlyph.Vector(Icons.Filled.Done)
  EntryAction.Notifications -> R.string.notifications_conversation to MenuGlyph.Vector(Icons.Filled.Notifications)
  EntryAction.CopyLink -> R.string.entry_copy_link to MenuGlyph.Vector(Icons.Filled.Share)
  EntryAction.LeaveChannel -> R.string.leave_channel to MenuGlyph.Vector(Icons.Filled.Close)
  EntryAction.DeleteChannel -> R.string.channel_delete to MenuGlyph.Vector(Icons.Filled.Delete)
  EntryAction.MuteSpace -> R.string.entry_mute_space to MenuGlyph.Drawable(R.drawable.ic_notifications_off)
  EntryAction.MarkSpaceRead -> R.string.entry_mark_all_read to MenuGlyph.Drawable(R.drawable.ic_done_all)
  EntryAction.LeaveSpace -> R.string.entry_leave_space to MenuGlyph.Vector(Icons.Filled.Close)
  EntryAction.DeleteSpace -> R.string.space_delete to MenuGlyph.Vector(Icons.Filled.Delete)
}

private fun isDestructive(action: EntryAction) =
  action == EntryAction.LeaveChannel ||
    action == EntryAction.DeleteChannel ||
    action == EntryAction.LeaveSpace ||
    action == EntryAction.DeleteSpace

/** What leaving means: the last member's leave deletes the channel, and the question says so. */
internal fun leaveText(leave: LeaveChoice) =
  if (leave == LeaveChoice.LAST_MEMBER) R.string.channel_leave_last_member_text else R.string.channel_leave_text

@Composable
private fun ConfirmDialog(title: String, text: String, confirmLabel: String, onConfirm: () -> Unit, onDismiss: () -> Unit) {
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(title) },
    text = { Text(text) },
    confirmButton = {
      TextButton(onClick = {
        onConfirm()
        onDismiss()
      }) { Text(confirmLabel, color = MaterialTheme.colorScheme.error) }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

private fun copyLink(context: android.content.Context, link: String) {
  context.getSystemService(ClipboardManager::class.java)?.setPrimaryClip(ClipData.newPlainText(link, link))
}
