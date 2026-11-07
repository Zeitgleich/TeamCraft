package de.zeitgleich.teamcraft.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.DraftItem
import de.zeitgleich.teamcraft.core.SentItem
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.conversationListName
import kotlinx.coroutines.flow.MutableStateFlow

/**
 * The composer a draft was opened for (#1220): it takes the focus once it stands, since the user
 * came to write.
 */
object OpenedDraft {
  data class Target(val conversation: String, val maybeThread: String?)

  val current = MutableStateFlow<Target?>(null)
}

/**
 * Every draft of the user and their recent messages, in two tabs (#1220). Both lists come from this
 * device: the drafts from the encrypted account data, the messages from the local store.
 */
@Composable
fun DraftsAndSentScreen(
  viewModel: MainViewModel,
  onOpenDraft: (DraftItem) -> Unit,
  onOpenSent: (SentItem) -> Unit,
  onBack: () -> Unit,
) {
  var tab by rememberSaveable { mutableIntStateOf(0) }
  val drafts = rememberRead(viewModel, enabled = tab == 0) { query(ChatQueries.drafts()).drafts }
  // The sent list is only read once it is looked at.
  val sent = rememberRead(viewModel, enabled = tab == 1) { query(ChatQueries.sentMessages()).sent }
  var maybeDeleting by remember { mutableStateOf<DraftItem?>(null) }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.drafts_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      PrimaryTabRow(selectedTabIndex = tab) {
        Tab(selected = tab == 0, onClick = { tab = 0 }, text = { Text(stringResource(R.string.drafts_tab_drafts)) })
        Tab(selected = tab == 1, onClick = { tab = 1 }, text = { Text(stringResource(R.string.drafts_tab_sent)) })
      }
      LazyColumn(Modifier.fillMaxSize()) {
        if (tab == 0) {
          placeholderItem(drafts.phase { it }, drafts.retry) { EmptyText(R.string.drafts_empty) }
          items(drafts.maybeValue.orEmpty(), key = { it.conversation + "/" + it.maybeThread.orEmpty() }) { draft ->
            DraftRow(draft, onOpen = { onOpenDraft(draft) }, onDelete = { maybeDeleting = draft })
            HorizontalDivider()
          }
        } else {
          val sentItems = sent.maybeValue.orEmpty()
          placeholderItem(sent.phase { it }, sent.retry) { EmptyText(R.string.sent_empty) }
          items(sentItems, key = { it.conversation + "/" + it.message }) { item ->
            SentRow(item, onOpen = { onOpenSent(item) })
            HorizontalDivider()
          }
          if (sentItems.isNotEmpty()) item { EmptyText(R.string.sent_note) }
        }
      }
    }
  }
  maybeDeleting?.let { draft ->
    AlertDialog(
      onDismissRequest = { maybeDeleting = null },
      title = { Text(stringResource(R.string.drafts_delete_question)) },
      confirmButton = {
        TextButton(onClick = {
          // The text and the files, on every device of the user (#1220).
          viewModel.act(ChatActions.discardDraft(draft.conversation, draft.maybeThread))
          maybeDeleting = null
        }) { Text(stringResource(R.string.drafts_delete)) }
      },
      dismissButton = { TextButton(onClick = { maybeDeleting = null }) { Text(stringResource(android.R.string.cancel)) } },
    )
  }
}

@Composable
private fun DraftRow(draft: DraftItem, onOpen: () -> Unit, onDelete: () -> Unit) {
  Row(Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(start = 16.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
    Column(Modifier.weight(1f)) {
      RowHeading(conversationListName(draft.conversationName, draft.isNotesToSelf, stringResource(R.string.notes_title)), draft.editedAt)
      draft.maybeThread?.let {
        Text(
          draft.maybeThreadRoot?.let { root -> stringResource(R.string.drafts_in_thread, root) } ?: stringResource(R.string.sent_in_thread),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
        )
      }
      if (draft.body.isNotEmpty()) Text(draft.body, maxLines = 3, overflow = TextOverflow.Ellipsis)
      if (draft.fileCount > 0) FileCount(draft.fileCount)
    }
    IconButton(onClick = onDelete) {
      Icon(Icons.Filled.Delete, contentDescription = stringResource(R.string.drafts_delete))
    }
  }
}

@Composable
private fun SentRow(item: SentItem, onOpen: () -> Unit) {
  Column(Modifier.fillMaxWidth().clickable(onClick = onOpen).padding(16.dp)) {
    RowHeading(conversationListName(item.conversationName, item.isNotesToSelf, stringResource(R.string.notes_title)), item.sentAt)
    if (item.maybeThread != null) {
      Text(
        stringResource(R.string.sent_in_thread),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (item.excerpt.isNotEmpty()) Text(item.excerpt, maxLines = 3, overflow = TextOverflow.Ellipsis)
    if (item.fileCount > 0) FileCount(item.fileCount)
  }
}

/** How many files a draft holds or a message carried. */
@Composable
private fun FileCount(count: Int) {
  Text(
    pluralStringResource(R.plurals.sent_files, count, count),
    style = MaterialTheme.typography.bodySmall,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
  )
}

@Composable
private fun RowHeading(name: String, at: Long) {
  val context = LocalContext.current
  Row {
    Text(
      name,
      style = MaterialTheme.typography.labelMedium,
      color = MaterialTheme.colorScheme.primary,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.weight(1f, fill = false).padding(end = 8.dp),
    )
    Text(
      DateUtils.getRelativeTimeSpanString(context, at).toString(),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
}

/** The pencil and the word for "a draft waits here", in the conversation and thread lists. */
@Composable
internal fun DraftLine(maybePreview: String?, modifier: Modifier = Modifier) {
  Row(modifier, verticalAlignment = Alignment.CenterVertically) {
    Icon(
      Icons.Filled.Edit,
      contentDescription = null,
      modifier = Modifier.size(14.dp),
      tint = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.width(4.dp))
    Text(
      maybePreview?.let { stringResource(R.string.draft_preview, it) } ?: stringResource(R.string.draft_marker),
      style = MaterialTheme.typography.bodyMedium,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
    )
  }
}
