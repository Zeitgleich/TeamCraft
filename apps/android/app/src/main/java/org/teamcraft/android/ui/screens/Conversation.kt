package org.teamcraft.android.ui.screens

import android.text.format.DateUtils
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import org.teamcraft.android.R
import org.teamcraft.android.ui.MainViewModel
import uniffi.teamcraft_ffi.ConversationItem
import uniffi.teamcraft_ffi.MessageItem

@Composable
fun ConversationScreen(
  viewModel: MainViewModel,
  conversationId: String,
  onOpenThread: (String) -> Unit,
  onBack: () -> Unit,
) {
  val changes by viewModel.changeCount.collectAsState()
  var conversation by remember { mutableStateOf<ConversationItem?>(null) }
  var messages by remember { mutableStateOf<List<MessageItem>>(emptyList()) }
  LaunchedEffect(changes, conversationId) {
    conversation = viewModel.conversations().firstOrNull { it.id == conversationId }
    messages = viewModel.messages(conversationId)
  }
  LaunchedEffect(conversationId, messages.size) { viewModel.markRead(conversationId) }
  Scaffold(
    topBar = {
      ChromeTopBar(
        title = conversation?.name.orEmpty(),
        maybeSubtitle = conversation?.topic,
        maybeOnBack = onBack,
      )
    },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding).imePadding()) {
      MessageList(messages, Modifier.weight(1f), onOpenThread = onOpenThread)
      if (conversation?.canPost != false) {
        Composer { body -> viewModel.send(conversationId, null, body) }
      } else {
        Text(
          stringResource(R.string.read_only_channel),
          color = MaterialTheme.colorScheme.onSurfaceVariant,
          modifier = Modifier.padding(16.dp),
        )
      }
    }
  }
}

@Composable
fun ThreadScreen(viewModel: MainViewModel, conversationId: String, threadId: String, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var messages by remember { mutableStateOf<List<MessageItem>>(emptyList()) }
  LaunchedEffect(changes, threadId) { messages = viewModel.thread(conversationId, threadId) }
  Scaffold(topBar = { ChromeTopBar(title = stringResource(R.string.thread_title), maybeOnBack = onBack) }) { padding ->
    Column(Modifier.fillMaxSize().padding(padding).imePadding()) {
      MessageList(messages, Modifier.weight(1f), onOpenThread = null)
      Composer { body -> viewModel.send(conversationId, threadId, body) }
    }
  }
}

@Composable
private fun MessageList(messages: List<MessageItem>, modifier: Modifier, onOpenThread: ((String) -> Unit)?) {
  val listState = rememberLazyListState()
  LaunchedEffect(messages.size) {
    if (messages.isNotEmpty()) listState.scrollToItem(messages.lastIndex)
  }
  LazyColumn(modifier.fillMaxWidth(), state = listState) {
    items(messages, key = { it.id }) { message ->
      MessageRow(message, onOpenThread)
      HorizontalDivider(color = MaterialTheme.colorScheme.outline.copy(alpha = 0.3f))
    }
  }
}

@Composable
fun MessageRow(message: MessageItem, onOpenThread: ((String) -> Unit)?) {
  val context = LocalContext.current
  Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp)) {
    Row(verticalAlignment = Alignment.Bottom) {
      Text(message.authorName, fontWeight = FontWeight.Bold, modifier = Modifier.padding(end = 8.dp))
      Text(
        if (message.isPending) {
          stringResource(R.string.pending)
        } else {
          DateUtils.formatDateTime(context, message.sentAtMs, DateUtils.FORMAT_SHOW_TIME)
        },
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    Text(message.body + if (message.isEdited) " " + stringResource(R.string.edited) else "")
    if (message.reactions.isNotEmpty()) {
      Text(
        message.reactions.joinToString("  ") { "${it.emoji} ${it.count}" },
        style = MaterialTheme.typography.bodySmall,
      )
    }
    if (onOpenThread != null && message.replyCount > 0u) {
      Text(
        pluralStringResource(R.plurals.replies, message.replyCount.toInt(), message.replyCount.toInt()),
        color = MaterialTheme.colorScheme.primary,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.clickable { onOpenThread(message.id) }.padding(top = 4.dp),
      )
    } else if (onOpenThread != null && !message.isPending) {
      Text(
        stringResource(R.string.reply_in_thread),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.clickable { onOpenThread(message.id) }.padding(top = 4.dp),
      )
    }
  }
}

@Composable
private fun Composer(onSend: (String) -> Unit) {
  var text by rememberSaveable { mutableStateOf("") }
  Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
    OutlinedTextField(
      value = text,
      onValueChange = { text = it },
      placeholder = { Text(stringResource(R.string.composer_hint)) },
      modifier = Modifier.weight(1f),
      maxLines = 6,
    )
    IconButton(
      onClick = {
        onSend(text.trim())
        text = ""
      },
      enabled = text.isNotBlank(),
    ) {
      Icon(Icons.AutoMirrored.Filled.Send, contentDescription = stringResource(R.string.send))
    }
  }
}
