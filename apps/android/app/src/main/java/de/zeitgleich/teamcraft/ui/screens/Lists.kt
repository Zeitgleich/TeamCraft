package de.zeitgleich.teamcraft.ui.screens

import androidx.compose.foundation.layout.consumeWindowInsets
import android.text.format.DateUtils
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Tab
import androidx.compose.material3.PrimaryTabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.text.style.TextAlign
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.conversationListName

/** Followed threads, newest reply first, with unread replies. */
@Composable
fun ThreadsScreen(viewModel: MainViewModel, onOpenThread: (String, String) -> Unit, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  val threads = rememberRead(viewModel) { query(ChatQueries.followedThreads()).threads }
  // The inbox is built from what the first sync brings: empty is no answer before that is here.
  val isSyncing = rememberNewestSyncing(viewModel)
  var draftThreads by remember { mutableStateOf<Set<Pair<String, String>>>(emptySet()) }
  LaunchedEffect(changes) {
    snapshot = viewModel.readChat { snapshot() }
    draftThreads = viewModel.readChat { query(ChatQueries.drafts()).drafts }.orEmpty()
      .mapNotNull { draft -> draft.maybeThread?.let { draft.conversation to it } }
      .toSet()
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.threads_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    LazyColumn(Modifier.fillMaxSize().consumeWindowInsets(padding), contentPadding = padding) {
      placeholderItem(threads.phase(isSyncing) { it }, threads.retry) { EmptyText(R.string.threads_empty) }
      items(threads.maybeValue.orEmpty(), key = { it.conversation + "/" + it.root.id }) { thread ->
        Column(Modifier.fillMaxWidth().clickable { onOpenThread(thread.conversation, thread.root.id) }.padding(16.dp)) {
          Text(
            snapshot?.conversation(thread.conversation)?.name.orEmpty(),
            style = MaterialTheme.typography.labelMedium,
            color = MaterialTheme.colorScheme.primary,
          )
          MessageLine(snapshot, thread.root)
          Text(
            pluralStringResource(R.plurals.replies, thread.replyCount, thread.replyCount) +
              if (thread.unreadReplies > 0) " · " + stringResource(R.string.unread_count, thread.unreadReplies) else "",
            style = MaterialTheme.typography.bodySmall,
            fontWeight = if (thread.unreadReplies > 0) FontWeight.Bold else FontWeight.Normal,
          )
          if ((thread.conversation to thread.root.id) in draftThreads) DraftLine(maybePreview = null)
        }
        HorizontalDivider()
      }
    }
  }
}

/** Saved items and recent mentions in two tabs. */
@Composable
fun SavedScreen(viewModel: MainViewModel, onOpen: (String) -> Unit, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var tab by rememberSaveable { mutableIntStateOf(0) }
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  val saved = rememberRead(viewModel) { query(ChatQueries.savedItems()).items }
  val mentions = rememberRead(viewModel) { query(ChatQueries.recentMentions()).hits }
  val isSyncing = rememberNewestSyncing(viewModel)
  LaunchedEffect(changes) { snapshot = viewModel.readChat { snapshot() } }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.saved_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(Modifier.fillMaxSize().padding(padding.withoutBottom()).consumeWindowInsets(padding)) {
      PrimaryTabRow(selectedTabIndex = tab) {
        Tab(selected = tab == 0, onClick = { tab = 0 }, text = { Text(stringResource(R.string.saved_items)) })
        Tab(selected = tab == 1, onClick = { tab = 1 }, text = { Text(stringResource(R.string.mentions)) })
      }
      LazyColumn(Modifier.fillMaxSize(), contentPadding = padding.bottomOnly()) {
        if (tab == 0) {
          // Saved items are the user's own, kept on this device: only a failed read is no answer.
          placeholderItem(saved.phase { it }, saved.retry) { EmptyText(R.string.saved_empty) }
          items(saved.maybeValue.orEmpty(), key = { it.message.id }) { item ->
            Column(Modifier.fillMaxWidth().clickable { onOpen(item.message.conversation) }.padding(16.dp)) {
              Text(conversationListName(item.conversationName, item.isNotesToSelf, stringResource(R.string.notes_title)), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
              MessageLine(snapshot, item.message)
            }
            HorizontalDivider()
          }
        } else {
          placeholderItem(mentions.phase(isSyncing) { it }, mentions.retry) { EmptyText(R.string.mentions_empty) }
          items(mentions.maybeValue.orEmpty(), key = { it.message.id }) { hit ->
            Column(Modifier.fillMaxWidth().clickable { onOpen(hit.message.conversation) }.padding(16.dp)) {
              Text(conversationListName(hit.conversationName, hit.isNotesToSelf, stringResource(R.string.notes_title)), style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
              MessageLine(snapshot, hit.message)
            }
            HorizontalDivider()
          }
        }
      }
    }
  }
}

@Composable
fun MessageLine(snapshot: ChatSnapshot?, message: Message) {
  val context = LocalContext.current
  Row {
    Text(snapshot?.name(message.author).orEmpty(), fontWeight = FontWeight.Bold, modifier = Modifier.padding(end = 8.dp))
    Text(
      DateUtils.getRelativeTimeSpanString(context, message.sentAt).toString(),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
  }
  Text(message.body, maxLines = 3, overflow = TextOverflow.Ellipsis)
}

@Composable
fun EmptyText(text: Int) {
  // Centred with room around it, like the other empty states, not a stray line in the corner.
  Text(
    stringResource(text),
    style = MaterialTheme.typography.bodyMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    textAlign = TextAlign.Center,
    modifier = Modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 48.dp),
  )
}
