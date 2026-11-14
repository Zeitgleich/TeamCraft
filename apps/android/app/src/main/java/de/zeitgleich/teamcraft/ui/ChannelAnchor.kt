package de.zeitgleich.teamcraft.ui

import androidx.annotation.StringRes
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import uniffi.teamcraft_ffi.ChannelAnchorForkItem
import uniffi.teamcraft_ffi.ChannelAnchorMarksItem
import uniffi.teamcraft_ffi.ChannelDeleteNameNote
import uniffi.teamcraft_ffi.ChannelNameProblem

// A channel's name rules and anchor (e2ee.md §4.6.5, issue #1842), as desktop and web show them.
// The core decides what to say; these only pick the strings.

/** The text at the name field for a name that breaks the rules; an empty one is the form's own. */
@StringRes
fun channelNameProblemText(problem: ChannelNameProblem): Int? = when (problem) {
  ChannelNameProblem.EMPTY -> null
  ChannelNameProblem.BIDI_CONTROL -> R.string.channel_name_error_bidi
  ChannelNameProblem.CONTROL_CHARACTER -> R.string.channel_name_error_control
  ChannelNameProblem.INVISIBLE_CHARACTER -> R.string.channel_name_error_invisible
}

/**
 * The note a channel's deletion confirmation adds about its name: the string, and the name it takes
 * where it names somebody (`null`: the person nobody can name, "Unknown person").
 */
data class DeleteNoteText(@StringRes val text: Int, val takesName: Boolean, val maybeName: String?)

fun channelDeleteNoteText(note: ChannelDeleteNameNote): DeleteNoteText = when (note) {
  ChannelDeleteNameNote.NameStale -> DeleteNoteText(R.string.channel_delete_name_stale, takesName = false, maybeName = null)
  is ChannelDeleteNameNote.NamedByFounder -> DeleteNoteText(R.string.channel_delete_named_by_founder, true, note.maybeName)
  is ChannelDeleteNameNote.RenamedByWorkspace -> DeleteNoteText(R.string.channel_delete_renamed_by_workspace, true, note.maybeName)
  is ChannelDeleteNameNote.RenamedByChannelAdmin ->
    DeleteNoteText(R.string.channel_delete_renamed_by_channel_admin, true, note.maybeName)
  is ChannelDeleteNameNote.RenamedByMember -> DeleteNoteText(R.string.channel_delete_renamed_by_member, true, note.maybeName)
  is ChannelDeleteNameNote.RenamedBy -> DeleteNoteText(R.string.channel_delete_renamed_by, true, note.maybeName)
}

/** A workspace admin's note on a forked anchor, by its side. */
@StringRes
fun channelAnchorForkText(fork: ChannelAnchorForkItem): Int = when (fork) {
  ChannelAnchorForkItem.FOUNDING -> R.string.channel_anchor_forked_founding
  ChannelAnchorForkItem.PROPERTIES -> R.string.channel_anchor_forked_properties
  ChannelAnchorForkItem.UNKNOWN -> R.string.channel_anchor_forked
}

/** The deletion confirmation's text with the note on the name, if this device holds one. */
@Composable
fun channelDeleteText(maybeMarks: ChannelAnchorMarksItem?): String {
  val text = stringResource(R.string.channel_delete_text)
  val note = maybeMarks?.maybeDeleteNote?.let(::channelDeleteNoteText) ?: return text
  val noteText = if (note.takesName) {
    stringResource(note.text, note.maybeName ?: stringResource(R.string.channel_roles_review_unknown))
  } else {
    stringResource(note.text)
  }
  return "$text $noteText"
}

/**
 * The channel's anchor marks while [isShown]: verified again first (at most every 5 minutes per
 * channel, the core keeps the pace), then read on every change. `null` for a DM or nothing held.
 */
@Composable
fun rememberChannelAnchorMarks(viewModel: MainViewModel, conversationId: String, isShown: Boolean): ChannelAnchorMarksItem? {
  val changes by viewModel.changeCount.collectAsState()
  var maybeMarks by remember(conversationId) { mutableStateOf<ChannelAnchorMarksItem?>(null) }
  LaunchedEffect(conversationId, isShown) { if (isShown) viewModel.refreshChannelAnchor(conversationId) }
  LaunchedEffect(conversationId, isShown, changes) {
    if (isShown) maybeMarks = viewModel.channelAnchorMarks(conversationId)
  }
  return maybeMarks
}

/**
 * The server offered this device a group the user's devices didn't pin for this channel: it can't
 * be read here (`anchor_refused`).
 */
@Composable
fun ChannelUnreachableBanner(viewModel: MainViewModel, conversationId: String) {
  val changes by viewModel.changeCount.collectAsState()
  var isUnreachable by remember(conversationId) { mutableStateOf(false) }
  LaunchedEffect(conversationId, changes) { isUnreachable = viewModel.isChannelUnreachable(conversationId) }
  if (!isUnreachable) return
  Surface(color = MaterialTheme.colorScheme.errorContainer, modifier = Modifier.fillMaxWidth()) {
    Text(
      stringResource(R.string.anchor_refused),
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.onErrorContainer,
      modifier = Modifier
        .padding(horizontal = 16.dp, vertical = 6.dp)
        .semantics { liveRegion = LiveRegionMode.Polite },
    )
  }
}
