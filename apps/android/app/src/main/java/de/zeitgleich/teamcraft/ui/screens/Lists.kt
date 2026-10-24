package de.zeitgleich.teamcraft.ui.screens

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
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.SavedItem
import de.zeitgleich.teamcraft.core.SearchHit
import de.zeitgleich.teamcraft.core.ThreadSummary
import de.zeitgleich.teamcraft.ui.MainViewModel

/** Followed threads, newest reply first, with unread replies. */
@Composable
fun ThreadsScreen(viewModel: MainViewModel, onOpenThread: (String, String) -> Unit, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var threads by remember { mutableStateOf<List<ThreadSummary>?>(null) }
  LaunchedEffect(changes) {
    snapshot = viewModel.readChat { snapshot() }
    threads = viewModel.readChat { query(ChatQueries.followedThreads()).threads }
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.threads_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    LazyColumn(Modifier.fillMaxSize().padding(padding)) {
      val list = threads.orEmpty()
      if (threads != null && list.isEmpty()) item { EmptyText(R.string.threads_empty) }
      items(list, key = { it.conversation + "/" + it.root.id }) { thread ->
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
  var saved by remember { mutableStateOf<List<SavedItem>>(emptyList()) }
  var mentions by remember { mutableStateOf<List<SearchHit>>(emptyList()) }
  LaunchedEffect(changes) {
    snapshot = viewModel.readChat { snapshot() }
    saved = viewModel.readChat { query(ChatQueries.savedItems()).items } ?: emptyList()
    mentions = viewModel.readChat { query(ChatQueries.recentMentions()).hits } ?: emptyList()
  }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.saved_title), maybeOnBack = onBack) }, contentWindowInsets = ScreenInsets) { padding ->
    Column(Modifier.fillMaxSize().padding(padding)) {
      PrimaryTabRow(selectedTabIndex = tab) {
        Tab(selected = tab == 0, onClick = { tab = 0 }, text = { Text(stringResource(R.string.saved_items)) })
        Tab(selected = tab == 1, onClick = { tab = 1 }, text = { Text(stringResource(R.string.mentions)) })
      }
      LazyColumn(Modifier.fillMaxSize()) {
        if (tab == 0) {
          if (saved.isEmpty()) item { EmptyText(R.string.saved_empty) }
          items(saved, key = { it.message.id }) { item ->
            Column(Modifier.fillMaxWidth().clickable { onOpen(item.message.conversation) }.padding(16.dp)) {
              Text(item.conversationName, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
              MessageLine(snapshot, item.message)
            }
            HorizontalDivider()
          }
        } else {
          if (mentions.isEmpty()) item { EmptyText(R.string.mentions_empty) }
          items(mentions, key = { it.message.id }) { hit ->
            Column(Modifier.fillMaxWidth().clickable { onOpen(hit.message.conversation) }.padding(16.dp)) {
              Text(hit.conversationName, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
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
  Text(stringResource(text), color = MaterialTheme.colorScheme.onSurfaceVariant, modifier = Modifier.padding(16.dp))
}
