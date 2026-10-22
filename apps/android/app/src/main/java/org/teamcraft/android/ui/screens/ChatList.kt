package org.teamcraft.android.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import org.teamcraft.android.R
import org.teamcraft.android.ui.MainViewModel
import org.teamcraft.android.ui.SectionTitle
import org.teamcraft.android.ui.sections
import org.teamcraft.android.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.ConversationKind

@Composable
fun ChatListScreen(
  viewModel: MainViewModel,
  onOpen: (String) -> Unit,
  onSearch: () -> Unit,
  onSettings: () -> Unit,
  onNavigate: (String) -> Unit,
) {
  val changes by viewModel.changeCount.collectAsState()
  var conversations by remember { mutableStateOf<List<ConversationItem>?>(null) }
  LaunchedEffect(changes) { conversations = viewModel.conversations() }
  Scaffold(
    topBar = {
      ChromeTopBar(
        title = stringResource(R.string.chats_title),
        actions = {
          IconButton(onClick = onSearch) {
            Icon(Icons.Filled.Search, contentDescription = stringResource(R.string.search))
          }
          IconButton(onClick = onSettings) {
            Icon(Icons.Filled.Settings, contentDescription = stringResource(R.string.settings_title))
          }
        },
      )
    },
  ) { outer ->
    androidx.compose.foundation.layout.Column(Modifier.fillMaxSize().padding(outer)) {
    androidx.compose.foundation.lazy.LazyRow(
      Modifier.fillMaxWidth().padding(horizontal = 8.dp, vertical = 4.dp),
      horizontalArrangement = androidx.compose.foundation.layout.Arrangement.spacedBy(8.dp),
    ) {
      for ((route, label) in QUICK_ROUTES) {
        item(key = route) {
          androidx.compose.material3.AssistChip(onClick = { onNavigate(route) }, label = { Text(stringResource(label)) })
        }
      }
    }
    val padding = androidx.compose.foundation.layout.PaddingValues(0.dp)
    val loaded = conversations
    when {
      loaded == null -> LoadingScreen()
      loaded.isEmpty() -> Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
        Text(stringResource(R.string.no_conversations))
      }
      else -> LazyColumn(Modifier.fillMaxSize().padding(padding)) {
        for (section in sections(loaded)) {
          item(key = "section-${section.title}") { SectionHeader(section.title) }
          items(section.conversations, key = { it.id }) { conversation ->
            ConversationRow(conversation, onClick = { onOpen(conversation.id) })
          }
        }
      }
    }
    }
  }
}

@Composable
private fun SectionHeader(title: SectionTitle) {
  val text = when (title) {
    SectionTitle.Favorites -> stringResource(R.string.favorites)
    SectionTitle.Channels -> stringResource(R.string.channels)
    SectionTitle.DirectMessages -> stringResource(R.string.direct_messages)
    is SectionTitle.Space -> title.name
  }
  Text(
    text.uppercase(),
    style = MaterialTheme.typography.labelMedium,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.padding(start = 16.dp, top = 16.dp, bottom = 4.dp),
  )
}

@Composable
private fun ConversationRow(conversation: ConversationItem, onClick: () -> Unit) {
  val isUnread = conversation.unreadCount > 0u
  Row(
    modifier = Modifier
      .fillMaxWidth()
      .clickable(onClick = onClick)
      .padding(horizontal = 16.dp, vertical = 10.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Text(
      prefix(conversation.kind),
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(end = 8.dp),
    )
    Text(
      conversation.name,
      modifier = Modifier.weight(1f),
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      fontWeight = if (isUnread) FontWeight.Bold else FontWeight.Normal,
      color = if (conversation.isMuted) MaterialTheme.colorScheme.onSurfaceVariant else MaterialTheme.colorScheme.onSurface,
    )
    if (conversation.mentionCount > 0u) {
      Badge(conversation.mentionCount.toString())
    }
  }
}

@Composable
private fun Badge(text: String) {
  Text(
    text,
    color = LocalTeamCraftColors.current.chromeText,
    style = MaterialTheme.typography.labelSmall,
    modifier = Modifier
      .background(LocalTeamCraftColors.current.badge, RoundedCornerShape(10.dp))
      .padding(horizontal = 7.dp, vertical = 2.dp),
  )
}

/** The sidebar's markers: `#` public, lock-like `◆` private, `@` direct. */
private fun prefix(kind: ConversationKind): String = when (kind) {
  ConversationKind.PUBLIC -> "#"
  ConversationKind.PRIVATE -> "◆"
  ConversationKind.DIRECT -> "@"
  ConversationKind.GROUP -> "@"
  ConversationKind.INTEGRATION -> "⚙"
}

/** Entry points above the conversation list. */
private val QUICK_ROUTES = listOf(
  "threads" to R.string.threads_title,
  "saved" to R.string.saved_title,
  "browse" to R.string.browse_title,
  "meetings" to R.string.meetings_title,
)
