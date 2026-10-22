package de.zeitgleich.teamcraft.ui.screens

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import android.text.format.DateUtils
import android.text.format.Formatter
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.calls.CallOptions
import de.zeitgleich.teamcraft.core.Bookmark
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.LinkPreview
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.SendOptions
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.Mentions
import uniffi.teamcraft_ffi.needsChannelConfirmation

/** Quick reactions in the action sheet. */
private val QUICK_REACTIONS = listOf("👍", "❤️", "😂", "🎉", "😮", "🙏")
private const val PAGE = 200

@Composable
fun ConversationScreen(
  viewModel: MainViewModel,
  conversationId: String,
  onOpenThread: (String) -> Unit,
  onOpenFiles: () -> Unit,
  onBack: () -> Unit,
) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var messages by remember { mutableStateOf<List<Message>>(emptyList()) }
  var extras by remember { mutableStateOf<Map<String, MessageExtras>>(emptyMap()) }
  var pinned by remember { mutableStateOf<List<Message>>(emptyList()) }
  var maybeFirstUnread by remember { mutableStateOf<String?>(null) }
  LaunchedEffect(conversationId) {
    // Where the user stopped reading, before marking the conversation read.
    maybeFirstUnread = viewModel.readChat { query(ChatQueries.firstUnread(conversationId)).maybeMessage }
  }
  LaunchedEffect(changes, conversationId) {
    snapshot = viewModel.readChat { snapshot() }
    messages = viewModel.readChat { latest(conversationId, PAGE).messages } ?: emptyList()
    extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: emptyMap()
    pinned = viewModel.readChat { query(ChatQueries.pinned(conversationId)).messages } ?: emptyList()
  }
  LaunchedEffect(conversationId, messages.size) { viewModel.markRead(conversationId) }
  val conversation = snapshot?.conversation(conversationId)
  ChatScaffold(
    viewModel = viewModel,
    title = conversation?.name.orEmpty(),
    maybeSubtitle = conversation?.topic,
    onBack = onBack,
    actions = {
      ConversationMenu(viewModel, conversationId, conversation, onOpenFiles)
    },
  ) {
    if (pinned.isNotEmpty()) PinnedBanner(pinned.last())
    MessageList(
      viewModel = viewModel,
      conversationId = conversationId,
      snapshot = snapshot,
      messages = messages,
      extras = extras,
      maybeFirstUnread = maybeFirstUnread,
      onOpenThread = onOpenThread,
      modifier = Modifier.weight(1f),
    )
    if (conversation?.canPost != false) {
      Composer(viewModel, conversationId, maybeThread = null, snapshot = snapshot)
    } else {
      Text(
        stringResource(R.string.read_only_channel),
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(16.dp),
      )
    }
  }
}

@Composable
fun ThreadScreen(viewModel: MainViewModel, conversationId: String, threadId: String, onBack: () -> Unit) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var messages by remember { mutableStateOf<List<Message>>(emptyList()) }
  var extras by remember { mutableStateOf<Map<String, MessageExtras>>(emptyMap()) }
  var isFollowed by remember { mutableStateOf(false) }
  LaunchedEffect(changes, threadId) {
    snapshot = viewModel.readChat { snapshot() }
    messages = viewModel.readChat { query(ChatQueries.thread(conversationId, threadId)).messages } ?: emptyList()
    extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: emptyMap()
    isFollowed = viewModel.readChat { query(ChatQueries.isThreadFollowed(conversationId, threadId)).value } ?: false
  }
  LaunchedEffect(threadId, messages.size) { viewModel.act(ChatActions.markThreadRead(conversationId, threadId)) }
  ChatScaffold(
    viewModel = viewModel,
    title = stringResource(R.string.thread_title),
    maybeSubtitle = snapshot?.conversation(conversationId)?.name,
    onBack = onBack,
    actions = {
      TextButton(onClick = { viewModel.act(ChatActions.setThreadFollowed(conversationId, threadId, !isFollowed)) }) {
        Text(stringResource(if (isFollowed) R.string.thread_unfollow else R.string.thread_follow))
      }
    },
  ) {
    MessageList(
      viewModel = viewModel,
      conversationId = conversationId,
      snapshot = snapshot,
      messages = messages,
      extras = extras,
      maybeFirstUnread = null,
      onOpenThread = null,
      modifier = Modifier.weight(1f),
    )
    Composer(viewModel, conversationId, maybeThread = threadId, snapshot = snapshot)
  }
}

/** Top bar, failure snackbar and the column the screens fill. */
@Composable
private fun ChatScaffold(
  viewModel: MainViewModel,
  title: String,
  maybeSubtitle: String?,
  onBack: () -> Unit,
  actions: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit,
  content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
  val snackbars = remember { SnackbarHostState() }
  val maybeFailure by viewModel.lastFailure.collectAsState()
  val context = LocalContext.current
  LaunchedEffect(maybeFailure) {
    maybeFailure?.let {
      snackbars.showSnackbar(context.getString(it))
      viewModel.failureShown()
    }
  }
  Scaffold(
    topBar = { ChromeTopBar(title = title, maybeSubtitle = maybeSubtitle, maybeOnBack = onBack, actions = actions) },
    snackbarHost = { SnackbarHost(snackbars) },
  ) { padding ->
    Column(Modifier.fillMaxSize().padding(padding).imePadding(), content = content)
  }
}

@Composable
private fun ConversationMenu(
  viewModel: MainViewModel,
  conversationId: String,
  maybeConversation: Conversation?,
  onOpenFiles: () -> Unit,
) {
  val isFavorite = maybeConversation?.isFavorite == true
  val name = maybeConversation?.name.orEmpty()
  val isChannel = maybeConversation?.isDirect == false
  var isOpen by remember { mutableStateOf(false) }
  var isNotifying by remember { mutableStateOf(false) }
  var isShowingBookmarks by remember { mutableStateOf(false) }
  var maybeConfirming by remember { mutableStateOf<ChannelConfirmation?>(null) }
  var isPreparingCall by remember { mutableStateOf(false) }
  IconButton(onClick = { isPreparingCall = true }) {
    Icon(Icons.Filled.Call, contentDescription = stringResource(R.string.call_start))
  }
  if (isPreparingCall) {
    PreJoinDialog(name, onDismiss = { isPreparingCall = false }) { options ->
      isPreparingCall = false
      viewModel.startCall(conversationId, name, options)
    }
  }
  IconButton(onClick = { viewModel.act(ChatActions.setFavorite(conversationId, !isFavorite)) }) {
    Icon(
      Icons.Filled.Star,
      contentDescription = stringResource(if (isFavorite) R.string.favorite_remove else R.string.favorite_add),
      tint = if (isFavorite) MaterialTheme.colorScheme.tertiary else androidx.compose.ui.graphics.Color.Unspecified,
    )
  }
  IconButton(onClick = { isOpen = true }) {
    Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.more))
  }
  DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
    DropdownMenuItem(text = { Text(stringResource(R.string.files_title)) }, onClick = {
      isOpen = false
      onOpenFiles()
    })
    DropdownMenuItem(text = { Text(stringResource(R.string.channel_bookmarks)) }, onClick = {
      isOpen = false
      isShowingBookmarks = true
    })
    DropdownMenuItem(text = { Text(stringResource(R.string.notifications_conversation)) }, onClick = {
      isOpen = false
      isNotifying = true
    })
    if (isChannel) {
      DropdownMenuItem(text = { Text(stringResource(R.string.channel_archive)) }, onClick = {
        isOpen = false
        maybeConfirming = ChannelConfirmation.Archive
      })
    }
    DropdownMenuItem(text = { Text(stringResource(R.string.leave_channel)) }, onClick = {
      isOpen = false
      maybeConfirming = ChannelConfirmation.Leave
    })
  }
  if (isNotifying) ConversationNotificationsDialog(viewModel, conversationId) { isNotifying = false }
  if (isShowingBookmarks) {
    BookmarksDialog(
      bookmarks = maybeConversation?.bookmarks.orEmpty(),
      onOpenFiles = onOpenFiles,
      onDismiss = { isShowingBookmarks = false },
    )
  }
  maybeConfirming?.let { confirmation ->
    ChannelConfirmationDialog(
      confirmation = confirmation,
      name = name,
      onConfirm = {
        when (confirmation) {
          ChannelConfirmation.Archive -> viewModel.act(ChatActions.archiveChannel(conversationId))
          ChannelConfirmation.Leave -> viewModel.act(ChatActions.leaveChannel(conversationId))
        }
      },
      onDismiss = { maybeConfirming = null },
    )
  }
}

/** A channel menu action that asks before it runs. */
private enum class ChannelConfirmation { Archive, Leave }

@Composable
private fun ChannelConfirmationDialog(
  confirmation: ChannelConfirmation,
  name: String,
  onConfirm: () -> Unit,
  onDismiss: () -> Unit,
) {
  val (title, text, confirmLabel) = when (confirmation) {
    ChannelConfirmation.Archive -> Triple(
      stringResource(R.string.channel_archive_title, name),
      stringResource(R.string.channel_archive_text),
      stringResource(R.string.channel_archive_confirm),
    )
    ChannelConfirmation.Leave -> Triple(
      stringResource(R.string.channel_leave_title, name),
      stringResource(R.string.channel_leave_text),
      stringResource(R.string.channel_leave_confirm),
    )
  }
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

/** The conversation's bookmarks: links open in the browser, files open the Files screen. */
@Composable
private fun BookmarksDialog(bookmarks: List<Bookmark>, onOpenFiles: () -> Unit, onDismiss: () -> Unit) {
  val context = LocalContext.current
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.channel_bookmarks)) },
    text = {
      if (bookmarks.isEmpty()) {
        Text(stringResource(R.string.channel_bookmarks_empty))
      } else {
        LazyColumn(Modifier.heightIn(max = 400.dp)) {
          items(bookmarks) { bookmark ->
            BookmarkRow(bookmark) {
              onDismiss()
              if (bookmark.isLink) openLink(context, bookmark.url) else onOpenFiles()
            }
          }
        }
      }
    },
    confirmButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.ok)) } },
  )
}

@Composable
private fun BookmarkRow(bookmark: Bookmark, onClick: () -> Unit) {
  Column(Modifier.fillMaxWidth().clickable(onClick = onClick).padding(vertical = 8.dp)) {
    Text(
      listOf(bookmark.emoji, bookmark.title).filter { it.isNotBlank() }.joinToString(" "),
      style = MaterialTheme.typography.bodyLarge,
    )
    Text(
      if (bookmark.isLink) bookmark.url else stringResource(R.string.channel_bookmark_file),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
    )
  }
}

private fun openLink(context: Context, url: String) {
  try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
  } catch (failure: ActivityNotFoundException) {
    // No app handles the link (or it is malformed); nothing sensible to open, so leave it.
  }
}

@Composable
private fun PinnedBanner(message: Message) {
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Text(
      "📌 " + message.body,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.padding(horizontal = 16.dp, vertical = 6.dp),
      style = MaterialTheme.typography.bodySmall,
    )
  }
}

@Composable
private fun MessageList(
  viewModel: MainViewModel,
  conversationId: String,
  snapshot: ChatSnapshot?,
  messages: List<Message>,
  extras: Map<String, MessageExtras>,
  maybeFirstUnread: String?,
  onOpenThread: ((String) -> Unit)?,
  modifier: Modifier,
) {
  val listState = rememberLazyListState()
  var maybeSelected by remember { mutableStateOf<Message?>(null) }
  LaunchedEffect(messages.size) {
    if (messages.isEmpty()) return@LaunchedEffect
    val unreadIndex = maybeFirstUnread?.let { id -> messages.indexOfFirst { it.id == id } } ?: -1
    listState.scrollToItem(if (unreadIndex >= 0) unreadIndex else messages.lastIndex)
  }
  LazyColumn(modifier.fillMaxWidth(), state = listState) {
    items(messages, key = { it.id }) { message ->
      if (message.id == maybeFirstUnread) UnreadDivider()
      MessageRow(
        viewModel = viewModel,
        message = message,
        maybeExtras = extras[message.id],
        snapshot = snapshot,
        onOpenThread = onOpenThread,
        onLongPress = { maybeSelected = message },
      )
      HorizontalDivider(color = MaterialTheme.colorScheme.outline.copy(alpha = 0.3f))
    }
  }
  maybeSelected?.let { message ->
    MessageActions(
      viewModel = viewModel,
      conversationId = conversationId,
      message = message,
      maybeExtras = extras[message.id],
      snapshot = snapshot,
      onOpenThread = onOpenThread,
      onDismiss = { maybeSelected = null },
    )
  }
}

@Composable
private fun UnreadDivider() {
  Text(
    stringResource(R.string.new_messages),
    color = MaterialTheme.colorScheme.error,
    style = MaterialTheme.typography.labelSmall,
    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp),
  )
}

@OptIn(ExperimentalFoundationApi::class, ExperimentalLayoutApi::class)
@Composable
fun MessageRow(
  viewModel: MainViewModel,
  message: Message,
  maybeExtras: MessageExtras?,
  snapshot: ChatSnapshot?,
  onOpenThread: ((String) -> Unit)?,
  onLongPress: () -> Unit,
) {
  val context = LocalContext.current
  val isMine = message.author == snapshot?.me?.id
  Column(
    Modifier
      .fillMaxWidth()
      .combinedClickable(onClick = {}, onLongClick = onLongPress)
      .padding(horizontal = 16.dp, vertical = 8.dp),
  ) {
    maybeExtras?.maybeForwarded?.let { forwarded ->
      Text(
        stringResource(R.string.forwarded_from, forwarded.authorName),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    maybeExtras?.maybeReply?.let { reply ->
      Text(
        "↪ ${snapshot?.name(reply.author) ?: ""}: ${reply.excerpt}",
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    Row(verticalAlignment = Alignment.Bottom) {
      Text(
        if (isMine) stringResource(R.string.you) else snapshot?.name(message.author).orEmpty(),
        fontWeight = FontWeight.Bold,
        modifier = Modifier.padding(end = 8.dp),
      )
      Text(
        if (message.isPending) {
          stringResource(R.string.pending)
        } else {
          DateUtils.formatDateTime(context, message.sentAt, DateUtils.FORMAT_SHOW_TIME)
        },
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      if (maybeExtras?.isPinned == true) Text(" 📌", style = MaterialTheme.typography.bodySmall)
      if (maybeExtras?.priority == "urgent") {
        Text(" " + stringResource(R.string.priority_urgent), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
      } else if (maybeExtras?.priority == "important") {
        Text(" " + stringResource(R.string.priority_important), color = MaterialTheme.colorScheme.tertiary, style = MaterialTheme.typography.labelSmall)
      }
    }
    when {
      maybeExtras?.isDeleted == true -> Text(stringResource(R.string.message_deleted), fontStyle = FontStyle.Italic)
      message.maybeSystemEvent != null || maybeExtras?.maybeWorkspaceEvent != null ->
        Text(systemText(message, maybeExtras, snapshot), fontStyle = FontStyle.Italic, color = MaterialTheme.colorScheme.onSurfaceVariant)
      message.body.isNotEmpty() -> Text(message.body + if (message.isEdited) " " + stringResource(R.string.edited) else "")
    }
    maybeExtras?.files?.forEach { file -> FileRow(viewModel, file) }
    maybeExtras?.maybeLinkPreview?.let { LinkPreviewCard(it) }
    if (message.reactions.isNotEmpty()) {
      FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        for (reaction in message.reactions) {
          AssistChip(
            onClick = { viewModel.act(ChatActions.react(message.conversation, message.id, reaction.emoji)) },
            label = { Text("${reaction.emoji} ${reaction.count}") },
          )
        }
      }
    }
    if (maybeExtras?.wantsAcknowledgement == true) {
      val count = maybeExtras.acknowledgedBy.size
      Text(
        pluralStringResource(R.plurals.acknowledged_by, count, count),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    if (onOpenThread != null && message.replyCount > 0) {
      Text(
        pluralStringResource(R.plurals.replies, message.replyCount, message.replyCount),
        color = MaterialTheme.colorScheme.primary,
        style = MaterialTheme.typography.bodySmall,
        modifier = Modifier.clickable { onOpenThread(message.id) }.padding(top = 4.dp),
      )
    }
  }
}

@Composable
private fun systemText(message: Message, maybeExtras: MessageExtras?, snapshot: ChatSnapshot?): String {
  val who = snapshot?.name(message.author).orEmpty()
  val event = message.maybeSystemEvent?.toString().orEmpty()
  val workspace = maybeExtras?.maybeWorkspaceEvent?.get("event")?.toString()?.trim('"').orEmpty()
  return when {
    event.contains("Joined") -> stringResource(R.string.event_joined, who)
    event.contains("Left") -> stringResource(R.string.event_left, who)
    event.contains("Renamed") -> stringResource(R.string.event_renamed, who)
    event.contains("TopicChanged") -> stringResource(R.string.event_topic, who)
    workspace == "archived" -> stringResource(R.string.event_archived, who)
    workspace == "made_private" -> stringResource(R.string.event_private, who)
    workspace == "made_public" -> stringResource(R.string.event_public, who)
    else -> stringResource(R.string.event_changed, who)
  }
}

@Composable
private fun FileRow(viewModel: MainViewModel, file: FileInfo) {
  val context = LocalContext.current
  Column(
    Modifier
      .padding(top = 4.dp)
      .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
      .clickable {
        val path = file.transfer.path
        if (file.transfer.state == "local" && path != null) {
          openFile(context, File(path), file.mediaType)
        } else {
          viewModel.act(ChatActions.download(file.conversation, file.message))
        }
      }
      .padding(8.dp),
  ) {
    file.thumbnailBytes()?.let { bytes ->
      BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.let { bitmap ->
        Image(bitmap.asImageBitmap(), contentDescription = file.fileName, modifier = Modifier.heightIn(max = 160.dp))
      }
    }
    Text("📎 ${file.fileName} · ${Formatter.formatShortFileSize(context, file.sizeBytes)}")
    file.transfer.maybeProgress?.let { progress ->
      LinearProgressIndicator(progress = { progress }, modifier = Modifier.width(160.dp).padding(top = 4.dp))
    }
    if (file.transfer.state == "failed") {
      Text(stringResource(R.string.file_failed), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
    }
  }
}

/** Opens a decrypted download in another app through the FileProvider. */
fun openFile(context: Context, file: File, mediaType: String) {
  val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
  val intent = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mediaType).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
  runCatching { context.startActivity(Intent.createChooser(intent, file.name)) }
}

@Composable
private fun LinkPreviewCard(preview: LinkPreview) {
  Column(
    Modifier
      .padding(top = 4.dp)
      .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
      .padding(8.dp),
  ) {
    preview.imageBytes()?.let { bytes ->
      BitmapFactory.decodeByteArray(bytes, 0, bytes.size)?.let { bitmap ->
        Image(bitmap.asImageBitmap(), contentDescription = null, modifier = Modifier.heightIn(max = 120.dp))
      }
    }
    preview.maybeSiteName?.let { Text(it, style = MaterialTheme.typography.labelSmall) }
    preview.maybeTitle?.let { Text(it, fontWeight = FontWeight.Bold) }
    preview.maybeDescription?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 3) }
  }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun MessageActions(
  viewModel: MainViewModel,
  conversationId: String,
  message: Message,
  maybeExtras: MessageExtras?,
  snapshot: ChatSnapshot?,
  onOpenThread: ((String) -> Unit)?,
  onDismiss: () -> Unit,
) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val isMine = message.author == snapshot?.me?.id
  var isEditing by remember { mutableStateOf(false) }
  var isForwarding by remember { mutableStateOf(false) }
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp)) {
      FlowRow(Modifier.padding(horizontal = 16.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        for (emoji in QUICK_REACTIONS) {
          TextButton(onClick = {
            viewModel.act(ChatActions.react(conversationId, message.id, emoji))
            onDismiss()
          }) { Text(emoji, style = MaterialTheme.typography.titleLarge) }
        }
      }
      SheetItem(R.string.action_reply) {
        ReplyTarget.set(conversationId, message, snapshot?.name(message.author).orEmpty())
        onDismiss()
      }
      if (onOpenThread != null) SheetItem(R.string.reply_in_thread) {
        onOpenThread(message.id)
        onDismiss()
      }
      if (isMine && maybeExtras?.isDeleted != true) {
        SheetItem(R.string.action_edit) { isEditing = true }
        SheetItem(R.string.action_delete) {
          viewModel.act(ChatActions.delete(conversationId, message.id))
          onDismiss()
        }
      }
      SheetItem(R.string.action_forward) { isForwarding = true }
      SheetItem(if (maybeExtras?.isPinned == true) R.string.action_unpin else R.string.action_pin) {
        viewModel.act(ChatActions.setPinned(conversationId, message.id, maybeExtras?.isPinned != true))
        onDismiss()
      }
      SheetItem(if (maybeExtras?.isSaved == true) R.string.action_unsave else R.string.action_save) {
        viewModel.act(ChatActions.setSaved(conversationId, message.id, maybeExtras?.isSaved != true))
        onDismiss()
      }
      if (maybeExtras?.wantsAcknowledgement == true && !isMine) SheetItem(R.string.action_acknowledge) {
        viewModel.act(ChatActions.acknowledge(conversationId, message.id))
        onDismiss()
      }
      SheetItem(R.string.action_copy_link) {
        scope.launch {
          viewModel.readChat { query(ChatQueries.permalink(conversationId, message.id)).maybeLink }?.let { link ->
            val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(ClipData.newPlainText("TeamCraft", link))
          }
          onDismiss()
        }
      }
      SheetItem(R.string.action_copy_text) {
        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        clipboard.setPrimaryClip(ClipData.newPlainText("TeamCraft", message.body))
        onDismiss()
      }
      SheetItem(R.string.action_mark_unread) {
        viewModel.act(ChatActions.markUnread(conversationId, message.id))
        onDismiss()
      }
      if (!maybeExtras?.edits.isNullOrEmpty()) {
        Text(
          stringResource(R.string.edit_history),
          style = MaterialTheme.typography.titleSmall,
          modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
        maybeExtras?.edits?.forEach { edit ->
          Text(edit.body, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 16.dp))
        }
      }
    }
  }
  if (isEditing) {
    TextDialog(
      title = R.string.action_edit,
      initial = message.body,
      onConfirm = { body ->
        viewModel.act(ChatActions.edit(conversationId, message.id, body))
        onDismiss()
      },
      onDismiss = { isEditing = false },
    )
  }
  if (isForwarding && snapshot != null) {
    ForwardDialog(snapshot, onPick = { target ->
      viewModel.act(ChatActions.forward(conversationId, message.id, target, ""))
      onDismiss()
    }, onDismiss = { isForwarding = false })
  }
}

@Composable
private fun SheetItem(label: Int, onClick: () -> Unit) {
  Text(
    stringResource(label),
    modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 24.dp, vertical = 14.dp),
  )
}

@Composable
fun TextDialog(title: Int, initial: String, onConfirm: (String) -> Unit, onDismiss: () -> Unit) {
  var text by remember { mutableStateOf(initial) }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(title)) },
    text = { OutlinedTextField(value = text, onValueChange = { text = it }) },
    confirmButton = { TextButton(onClick = { onConfirm(text) }, enabled = text.isNotBlank()) { Text(stringResource(R.string.ok)) } },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

@Composable
private fun ForwardDialog(snapshot: ChatSnapshot, onPick: (String) -> Unit, onDismiss: () -> Unit) {
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.action_forward)) },
    text = {
      LazyColumn(Modifier.heightIn(max = 360.dp)) {
        items(snapshot.conversations.filter { it.canPost }, key = { it.id }) { conversation ->
          Text(
            conversation.name,
            modifier = Modifier.fillMaxWidth().clickable { onPick(conversation.id) }.padding(vertical = 10.dp),
          )
        }
      }
    },
    confirmButton = {},
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** The message the composer of a conversation replies to (set from the action sheet). */
object ReplyTarget {
  data class Target(val conversation: String, val message: Message, val authorName: String)

  val current = kotlinx.coroutines.flow.MutableStateFlow<Target?>(null)

  fun set(conversation: String, message: Message, authorName: String) {
    current.value = Target(conversation, message, authorName)
  }
}

@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun Composer(viewModel: MainViewModel, conversationId: String, maybeThread: String?, snapshot: ChatSnapshot?) {
  val scope = rememberCoroutineScope()
  var text by rememberSaveable(conversationId, maybeThread) { mutableStateOf("") }
  var isDraftLoaded by remember(conversationId, maybeThread) { mutableStateOf(false) }
  var files by remember(conversationId, maybeThread) { mutableStateOf<List<PickedFile>>(emptyList()) }
  var priority by rememberSaveable(conversationId) { mutableStateOf("normal") }
  var wantsAcknowledgement by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isSilent by rememberSaveable(conversationId) { mutableStateOf(false) }
  var alsoToChannel by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isOptionsOpen by remember { mutableStateOf(false) }
  val replyTarget by ReplyTarget.current.collectAsState()
  val maybeReply = replyTarget?.takeIf { it.conversation == conversationId && maybeThread == null }
  val picker = rememberLauncherForActivityResult(ActivityResultContracts.GetMultipleContents()) { uris ->
    scope.launch { files = files + uris.mapNotNull { viewModel.importFile(it) } }
  }
  LaunchedEffect(conversationId, maybeThread) {
    // A draft from this or another device of the user.
    if (text.isEmpty()) text = viewModel.readChat { query(ChatQueries.draft(conversationId, maybeThread)).maybeBody }.orEmpty()
    isDraftLoaded = true
  }
  LaunchedEffect(text) {
    if (!isDraftLoaded) return@LaunchedEffect
    delay(DRAFT_SAVE_DELAY_MS)
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, text))
  }
  val maybeQuery = Mentions.activeQuery(text)
  val members = snapshot?.conversation(conversationId)?.memberIds?.mapNotNull { snapshot.user(it) } ?: emptyList()
  fun send() {
    val options = SendOptions(
      maybeThread = maybeThread,
      maybeReplyTo = maybeReply?.message?.id,
      priority = priority,
      wantsAcknowledgement = wantsAcknowledgement,
      isSilent = isSilent,
      alsoSendToChannel = alsoToChannel,
      files = files,
    )
    viewModel.act(ChatActions.send(conversationId, text.trim(), options))
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, ""))
    text = ""
    files = emptyList()
    priority = "normal"
    wantsAcknowledgement = false
    isSilent = false
    ReplyTarget.current.value = null
  }
  var isConfirmingChannel by remember { mutableStateOf(false) }
  if (isConfirmingChannel) {
    AlertDialog(
      onDismissRequest = { isConfirmingChannel = false },
      title = { Text(stringResource(R.string.channel_mention_confirm_title)) },
      text = { Text(pluralStringResource(R.plurals.channel_mention_confirm, members.size, members.size)) },
      confirmButton = {
        TextButton(onClick = {
          isConfirmingChannel = false
          send()
        }) { Text(stringResource(R.string.send)) }
      },
      dismissButton = { TextButton(onClick = { isConfirmingChannel = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
  Column(Modifier.fillMaxWidth()) {
    if (maybeQuery != null) {
      Mentions.suggestions(maybeQuery, members).forEach { user ->
        Text(
          "@${user.username} · ${user.displayName}",
          modifier = Modifier.fillMaxWidth().clickable { text = Mentions.complete(text, user) }.padding(horizontal = 16.dp, vertical = 8.dp),
        )
      }
    }
    maybeReply?.let { reply ->
      Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), verticalAlignment = Alignment.CenterVertically) {
        Text(
          stringResource(R.string.replying_to, reply.authorName) + ": " + reply.message.body,
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
          style = MaterialTheme.typography.bodySmall,
          modifier = Modifier.weight(1f),
        )
        IconButton(onClick = { ReplyTarget.current.value = null }) {
          Icon(Icons.Filled.Close, contentDescription = stringResource(R.string.cancel))
        }
      }
    }
    if (files.isNotEmpty()) {
      FlowRow(Modifier.padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        for (file in files) {
          AssistChip(onClick = { files = files - file }, label = { Text("📎 ${file.fileName} ✕") })
        }
      }
    }
    val flags = listOfNotNull(
      stringResource(R.string.priority_urgent).takeIf { priority == "urgent" },
      stringResource(R.string.priority_important).takeIf { priority == "important" },
      stringResource(R.string.option_acknowledge).takeIf { wantsAcknowledgement },
      stringResource(R.string.option_silent).takeIf { isSilent },
      stringResource(R.string.option_also_to_channel).takeIf { alsoToChannel },
    )
    if (flags.isNotEmpty()) {
      Text(flags.joinToString(" · "), style = MaterialTheme.typography.labelSmall, modifier = Modifier.padding(horizontal = 16.dp))
    }
    Row(Modifier.fillMaxWidth().padding(8.dp), verticalAlignment = Alignment.CenterVertically) {
      IconButton(onClick = { picker.launch("*/*") }) {
        Icon(Icons.Filled.Add, contentDescription = stringResource(R.string.attach))
      }
      OutlinedTextField(
        value = text,
        onValueChange = { text = it },
        placeholder = { Text(stringResource(R.string.composer_hint)) },
        modifier = Modifier.weight(1f),
        maxLines = 6,
      )
      Column {
        IconButton(onClick = { isOptionsOpen = true }) {
          Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.send_options))
        }
        DropdownMenu(expanded = isOptionsOpen, onDismissRequest = { isOptionsOpen = false }) {
          DropdownMenuItem(text = { Text(stringResource(R.string.priority_normal)) }, onClick = { priority = "normal"; isOptionsOpen = false })
          DropdownMenuItem(text = { Text(stringResource(R.string.priority_important)) }, onClick = { priority = "important"; isOptionsOpen = false })
          DropdownMenuItem(text = { Text(stringResource(R.string.priority_urgent)) }, onClick = { priority = "urgent"; isOptionsOpen = false })
          DropdownMenuItem(text = { Text(stringResource(R.string.option_acknowledge)) }, onClick = { wantsAcknowledgement = !wantsAcknowledgement; isOptionsOpen = false })
          DropdownMenuItem(text = { Text(stringResource(R.string.option_silent)) }, onClick = { isSilent = !isSilent; isOptionsOpen = false })
          if (maybeThread != null) {
            DropdownMenuItem(text = { Text(stringResource(R.string.option_also_to_channel)) }, onClick = { alsoToChannel = !alsoToChannel; isOptionsOpen = false })
          }
        }
      }
      IconButton(
        onClick = {
          val confirmOver = viewModel.confirmChannelOver().toUInt()
          if (needsChannelConfirmation(text, members.size.toUInt(), confirmOver)) isConfirmingChannel = true else send()
        },
        enabled = text.isNotBlank() || files.isNotEmpty(),
      ) {
        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = stringResource(R.string.send))
      }
    }
  }
}

/** Before a call: join with the camera on or muted. */
@Composable
private fun PreJoinDialog(name: String, onDismiss: () -> Unit, onJoin: (CallOptions) -> Unit) {
  var isCameraOn by rememberSaveable { mutableStateOf(false) }
  var isMuted by rememberSaveable { mutableStateOf(false) }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(R.string.call_prejoin_title, name)) },
    text = {
      Column {
        Row(verticalAlignment = Alignment.CenterVertically) {
          Switch(checked = isCameraOn, onCheckedChange = { isCameraOn = it })
          Text(stringResource(R.string.call_prejoin_camera), modifier = Modifier.padding(start = 8.dp))
        }
        Row(verticalAlignment = Alignment.CenterVertically) {
          Switch(checked = isMuted, onCheckedChange = { isMuted = it })
          Text(stringResource(R.string.call_prejoin_muted), modifier = Modifier.padding(start = 8.dp))
        }
      }
    },
    confirmButton = {
      TextButton(onClick = { onJoin(CallOptions(isCameraOn = isCameraOn, isMuted = isMuted)) }) {
        Text(stringResource(R.string.call_prejoin_join))
      }
    },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

/** Drafts are saved (and synced to the user's other devices) after typing pauses this long. */
private const val DRAFT_SAVE_DELAY_MS = 800L
