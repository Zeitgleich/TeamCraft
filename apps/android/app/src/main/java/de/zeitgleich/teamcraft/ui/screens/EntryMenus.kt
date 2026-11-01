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
  // Read when the menu opens, and again after the core has read the roles it was missing.
  LaunchedEffect(isOpen, conversation.id) {
    if (!isOpen) return@LaunchedEffect
    leave = viewModel.leaveChoice(conversation.id)
    maybeLink = viewModel.conversationLink(conversation.id)
  }
  DropdownMenu(expanded = isOpen, onDismissRequest = onDismiss) {
    for (entry in conversationMenu(conversation, leave, hasLink = maybeLink != null)) {
      EntryMenuItem(entry, lastAdminHint = R.string.entry_leave_last_channel_admin) {
        onDismiss()
        when (entry.action) {
          EntryAction.Select -> onSelect()
          EntryAction.MarkRead -> viewModel.markRead(conversation.id)
          EntryAction.Notifications -> onNotifications()
          EntryAction.CopyLink -> maybeLink?.let { copyLink(context, it) }
          EntryAction.LeaveChannel -> isConfirmingLeave = true
          else -> Unit
        }
      }
    }
  }
  if (isConfirmingLeave) {
    ConfirmDialog(
      title = stringResource(R.string.channel_leave_title, conversation.name),
      text = stringResource(R.string.channel_leave_text),
      confirmLabel = stringResource(R.string.channel_leave_confirm),
      onConfirm = { viewModel.act(ChatActions.leaveChannel(conversation.id)) },
      onDismiss = { isConfirmingLeave = false },
    )
  }
}

/** The long-press menu of a space's header: mute (its channels follow), mark all read, leave. */
@Composable
fun SpaceEntryMenu(viewModel: MainViewModel, spaceId: String, name: String, isOpen: Boolean, onDismiss: () -> Unit) {
  var maybeMenu by remember(spaceId) { mutableStateOf<uniffi.teamcraft_ffi.SpaceMenuItem?>(null) }
  var isMuting by remember(spaceId) { mutableStateOf(false) }
  var isConfirmingLeave by remember(spaceId) { mutableStateOf(false) }
  LaunchedEffect(isOpen, spaceId) {
    if (isOpen) maybeMenu = viewModel.spaceMenu(spaceId)
  }
  val menu = maybeMenu
  DropdownMenu(expanded = isOpen && menu != null, onDismissRequest = onDismiss) {
    if (menu != null) {
      for (entry in spaceMenu(menu.hasUnread, menu.leave)) {
        EntryMenuItem(entry, lastAdminHint = R.string.entry_leave_last_space_admin) {
          onDismiss()
          when (entry.action) {
            EntryAction.MuteSpace -> isMuting = true
            EntryAction.MarkSpaceRead -> viewModel.markSpaceRead(spaceId)
            EntryAction.LeaveSpace -> isConfirmingLeave = true
            else -> Unit
          }
        }
      }
    }
  }
  if (isMuting) SpaceMuteDialog(viewModel, spaceId, name) { isMuting = false }
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
  EntryAction.MuteSpace -> R.string.entry_mute_space to MenuGlyph.Drawable(R.drawable.ic_notifications_off)
  EntryAction.MarkSpaceRead -> R.string.entry_mark_all_read to MenuGlyph.Drawable(R.drawable.ic_done_all)
  EntryAction.LeaveSpace -> R.string.entry_leave_space to MenuGlyph.Vector(Icons.Filled.Close)
}

private fun isDestructive(action: EntryAction) = action == EntryAction.LeaveChannel || action == EntryAction.LeaveSpace

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
