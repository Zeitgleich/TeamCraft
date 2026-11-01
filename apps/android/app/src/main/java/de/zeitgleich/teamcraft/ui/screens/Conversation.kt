package de.zeitgleich.teamcraft.ui.screens

import androidx.annotation.StringRes
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.contentDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.graphics.Paint
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Call
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Notifications
import androidx.compose.material.icons.filled.Star
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.IconButtonDefaults
import androidx.compose.material3.InputChip
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextField
import androidx.compose.material3.TextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.isShiftPressed
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.FileProvider
import androidx.core.graphics.PaintCompat
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.ui.forms.fieldError
import de.zeitgleich.teamcraft.ui.forms.requiredTextError
import de.zeitgleich.teamcraft.ui.forms.supportingText
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.TeamCraftApplication
import de.zeitgleich.teamcraft.calls.CallOptions
import de.zeitgleich.teamcraft.core.Bookmark
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatQueries
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.Conversation
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.PickedFile
import de.zeitgleich.teamcraft.core.SendOptions
import de.zeitgleich.teamcraft.ui.ComposerPrefs
import de.zeitgleich.teamcraft.ui.ConversationRights
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.MessageAction
import de.zeitgleich.teamcraft.ui.messageActions
import de.zeitgleich.teamcraft.ui.EmojiInput
import de.zeitgleich.teamcraft.ui.FirstUnread
import de.zeitgleich.teamcraft.ui.Mentions
import de.zeitgleich.teamcraft.ui.PagedFeed
import de.zeitgleich.teamcraft.ui.PendingShare
import de.zeitgleich.teamcraft.ui.TypingLine
import de.zeitgleich.teamcraft.ui.shouldNotifyTyping
import de.zeitgleich.teamcraft.ui.typingLine
import de.zeitgleich.teamcraft.ui.entry
import de.zeitgleich.teamcraft.ui.sensitiveClip
import de.zeitgleich.teamcraft.ui.requestsIn
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.NotifiedConversation
import uniffi.teamcraft_ffi.emojiConvert
import uniffi.teamcraft_ffi.emojiSuggestions
import uniffi.teamcraft_ffi.needsChannelConfirmation

/** Quick reactions in the action sheet. */
private val QUICK_REACTIONS = listOf("👍", "❤️", "😂", "🎉", "😮", "🙏")
private const val PAGE = 200
/** Messages each step back through the history adds (issue #1058). */
private const val PAGE_BACK = 100

@Composable
fun ConversationScreen(
  viewModel: MainViewModel,
  conversationId: String,
  onOpenThread: (String) -> Unit,
  onOpenFiles: () -> Unit,
  onBack: () -> Unit,
  maybeFocusMessage: String? = null,
  onOpenPermalink: (String) -> Unit = {},
) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var messages by remember { mutableStateOf<List<Message>>(emptyList()) }
  var extras by remember { mutableStateOf<Map<String, MessageExtras>>(emptyMap()) }
  var pinned by remember { mutableStateOf<List<Message>>(emptyList()) }
  // Admins asking to join this channel, when the user owns it (admin.md §2.4.1).
  var joinRequests by remember { mutableStateOf<List<JoinRequestItem>>(emptyList()) }
  var firstUnread by remember { mutableStateOf<FirstUnread>(FirstUnread.Loading) }
  // Counts sends from this screen's composer: each one brings the newest message into view.
  var sentCount by remember { mutableIntStateOf(0) }
  var undecrypted by remember { mutableStateOf(0) }
  // What the notification that opened it showed, until the store has it (issue #1065).
  var maybeNotified by remember(conversationId) { mutableStateOf<NotifiedConversation?>(null) }
  val feed = remember(conversationId) {
    PagedFeed(PAGE, PAGE_BACK) { maybeOldest, older -> viewModel.readChat { tail(conversationId, maybeOldest, older) } }
  }
  var hasOlder by remember(conversationId) { mutableStateOf(false) }
  var isLoadingOlder by remember(conversationId) { mutableStateOf(false) }
  val scope = rememberCoroutineScope()
  val rights = rememberRights(viewModel, conversationId, changes)
  LaunchedEffect(conversationId) {
    // Where the user stopped reading, before marking the conversation read.
    val maybeMessage = viewModel.readChat { query(ChatQueries.firstUnread(conversationId)).maybeMessage }
    firstUnread = maybeMessage?.let { FirstUnread.At(it) } ?: FirstUnread.None
  }
  LaunchedEffect(changes, conversationId) {
    snapshot = viewModel.readChat { snapshot() }
    val maybeTail = feed.reread()
    messages = maybeTail?.messages ?: emptyList()
    hasOlder = maybeTail?.hasOlder ?: false
    undecrypted = maybeTail?.undecrypted ?: 0
    // After the messages: once the store has the notified one, the core no longer names it.
    maybeNotified = viewModel.notifiedConversation(conversationId)
    extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: emptyMap()
    pinned = viewModel.readChat { query(ChatQueries.pinned(conversationId)).messages } ?: emptyList()
    joinRequests = requestsIn(conversationId, viewModel.joinRequests.list())
  }
  FollowTransfers(extras) { extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: extras }
  LaunchedEffect(conversationId, messages.size) { viewModel.markRead(conversationId) }
  // Receipts sent while this device was offline never arrive as events: they are read when the
  // conversation opens and whenever the connection comes back (#769). Channels have none, so the
  // core doesn't ask for theirs; a failure stays quiet, the next connection asks again.
  val maybeConnection by TeamCraftApplication.graph(LocalContext.current).core.connectionState.collectAsState()
  val isConnected = maybeConnection is ConnectionState.Connected || maybeConnection is ConnectionState.Slow
  LaunchedEffect(conversationId, isConnected) {
    if (isConnected) viewModel.readChat { act(ChatActions.refreshReadReceipts(conversationId)) }
  }
  val conversation = snapshot?.conversation(conversationId)
  ChatScaffold(
    viewModel = viewModel,
    topBar = {
      ConversationTopBar(snapshot, conversation, maybeNotified, onBack) {
        ConversationMenu(viewModel, conversationId, conversation, rights, onOpenFiles)
      }
    },
  ) {
    if (pinned.isNotEmpty()) PinnedBanner(pinned.last())
    JoinRequestBanner(joinRequests, viewModel.joinRequests)
    // Received but not readable here yet: without this the conversation would just look empty.
    if (undecrypted > 0) {
      Text(
        pluralStringResource(R.plurals.messages_undecrypted, undecrypted, undecrypted),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
      )
    }
    MessageList(
      viewModel = viewModel,
      conversationId = conversationId,
      snapshot = snapshot,
      messages = messages,
      extras = extras,
      firstUnread = firstUnread,
      sentCount = sentCount,
      onOpenThread = onOpenThread,
      maybeOnReply = { message: Message ->
        ReplyTarget.set(conversationId, message, snapshot?.name(message.author).orEmpty())
      }.takeIf { rights.canPost },
      rights = rights,
      modifier = Modifier.weight(1f),
      maybeFocusMessage = maybeFocusMessage,
      onOpenPermalink = onOpenPermalink,
      older = OlderMessages(hasOlder, isLoadingOlder) {
        if (isLoadingOlder) return@OlderMessages
        isLoadingOlder = true
        scope.launch {
          // A failed read leaves the rows as they are; the next scroll near the top asks again.
          feed.older()?.let { tail ->
            messages = tail.messages
            hasOlder = tail.hasOlder
            undecrypted = tail.undecrypted
          }
          isLoadingOlder = false
        }
      },
    )
    NotifiedMessageRow(maybeNotified, isLoading = conversation == null || messages.isEmpty())
    TypingIndicator(viewModel, conversationId, snapshot, changes)
    if (conversation?.canPost != false) {
      Composer(viewModel, conversationId, maybeThread = null, snapshot = snapshot, onSent = { sentCount++ })
    } else {
      ReadOnlyNote()
    }
  }
}

@Composable
fun ThreadScreen(
  viewModel: MainViewModel,
  conversationId: String,
  threadId: String,
  onBack: () -> Unit,
  maybeFocusMessage: String? = null,
  onOpenPermalink: (String) -> Unit = {},
) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf<ChatSnapshot?>(null) }
  var messages by remember { mutableStateOf<List<Message>>(emptyList()) }
  var extras by remember { mutableStateOf<Map<String, MessageExtras>>(emptyMap()) }
  var isFollowed by remember { mutableStateOf(false) }
  var sentCount by remember { mutableIntStateOf(0) }
  val rights = rememberRights(viewModel, conversationId, changes)
  LaunchedEffect(changes, threadId) {
    snapshot = viewModel.readChat { snapshot() }
    messages = viewModel.readChat { query(ChatQueries.thread(conversationId, threadId)).messages } ?: emptyList()
    extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: emptyMap()
    isFollowed = viewModel.readChat { query(ChatQueries.isThreadFollowed(conversationId, threadId)).value } ?: false
  }
  FollowTransfers(extras) { extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: extras }
  LaunchedEffect(threadId, messages.size) { viewModel.act(ChatActions.markThreadRead(conversationId, threadId)) }
  val threadTitle = stringResource(R.string.thread_title)
  ChatScaffold(
    viewModel = viewModel,
    topBar = {
      ChromeTopBar(title = threadTitle, maybeSubtitle = snapshot?.conversation(conversationId)?.name, maybeOnBack = onBack) {
        TextButton(onClick = { viewModel.act(ChatActions.setThreadFollowed(conversationId, threadId, !isFollowed)) }) {
          Text(stringResource(if (isFollowed) R.string.thread_unfollow else R.string.thread_follow))
        }
      }
    },
  ) {
    MessageList(
      viewModel = viewModel,
      conversationId = conversationId,
      snapshot = snapshot,
      messages = messages,
      extras = extras,
      firstUnread = FirstUnread.None,
      sentCount = sentCount,
      onOpenThread = null,
      maybeOnReply = null,
      rights = rights,
      modifier = Modifier.weight(1f),
      maybeFocusMessage = maybeFocusMessage,
      onOpenPermalink = onOpenPermalink,
    )
    TypingIndicator(viewModel, conversationId, snapshot, changes)
    if (rights.canPost) {
      Composer(viewModel, conversationId, maybeThread = threadId, snapshot = snapshot, onSent = { sentCount++ })
    } else {
      ReadOnlyNote()
    }
  }
}

/**
 * What the user may do in the conversation, read again with every change: a role or scheme change
 * on the server reaches the screen without reopening it (issue #758).
 */
@Composable
private fun rememberRights(viewModel: MainViewModel, conversationId: String, changes: Long): ConversationRights {
  var rights by remember(conversationId) { mutableStateOf(ConversationRights()) }
  LaunchedEffect(changes, conversationId) {
    rights = ConversationRights.of(viewModel.conversations().firstOrNull { it.id == conversationId })
  }
  return rights
}

/** In place of the composer where the user may only read. */
@Composable
private fun ReadOnlyNote() {
  Text(
    stringResource(R.string.read_only_channel),
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    modifier = Modifier.padding(16.dp),
  )
}

/**
 * While a file uploads or downloads, re-reads every second: the core reports a transfer's end in
 * the store but not every chunk, so progress and the finished download would not show otherwise.
 */
@Composable
private fun FollowTransfers(extras: Map<String, MessageExtras>, reload: suspend () -> Unit) {
  LaunchedEffect(extras) {
    while (extras.values.any { extra -> extra.files.any { it.transfer.isMoving } }) {
      delay(TRANSFER_POLL_MS)
      reload()
    }
  }
}

/**
 * "Alice is typing…" above the composer. Re-read on every change (a typing frame is one) and
 * every couple of seconds while someone types, since indicators run out without an event.
 */
@Composable
private fun TypingIndicator(viewModel: MainViewModel, conversationId: String, snapshot: ChatSnapshot?, changes: Long) {
  var typing by remember(conversationId) { mutableStateOf<List<String>>(emptyList()) }
  LaunchedEffect(conversationId, changes) {
    do {
      typing = viewModel.readChat { query(ChatQueries.typingUsers(conversationId)).users } ?: emptyList()
      if (typing.isNotEmpty()) delay(TYPING_POLL_MS)
    } while (typing.isNotEmpty())
  }
  val text = when (val line = typingLine(typing.map { snapshot?.name(it).orEmpty() })) {
    null -> return
    is TypingLine.One -> stringResource(R.string.typing_one, line.name)
    is TypingLine.Two -> stringResource(R.string.typing_two, line.first, line.second)
    TypingLine.Several -> stringResource(R.string.typing_several)
  }
  Text(
    text,
    style = MaterialTheme.typography.labelMedium,
    fontStyle = FontStyle.Italic,
    color = MaterialTheme.colorScheme.onSurfaceVariant,
    maxLines = 1,
    overflow = TextOverflow.Ellipsis,
    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 2.dp),
  )
}

/** Top bar, failure snackbar and the column the screens fill. */
@Composable
private fun ChatScaffold(
  viewModel: MainViewModel,
  topBar: @Composable () -> Unit,
  content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit,
) {
  val snackbars = rememberFailureSnackbars(viewModel)
  Scaffold(
    contentWindowInsets = ScreenInsets,
    topBar = topBar,
    snackbarHost = { SnackbarHost(snackbars) },
  ) { padding ->
    // The insets include the keyboard: the composer rides on top of it as it slides in (#309).
    Column(Modifier.fillMaxSize().padding(padding)) {
      OfflineBanner(onSignInAgain = { viewModel.signOut() })
      content()
    }
  }
}

@Composable
private fun ConversationMenu(
  viewModel: MainViewModel,
  conversationId: String,
  maybeConversation: Conversation?,
  rights: ConversationRights,
  onOpenFiles: () -> Unit,
) {
  val isFavorite = maybeConversation?.isFavorite == true
  val name = maybeConversation?.name.orEmpty()
  // Archiving changes the channel itself; DMs have none to archive.
  val canArchive = maybeConversation?.isDirect == false && rights.canManageChannel
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
  IconButton(onClick = { isNotifying = true }) {
    if (maybeConversation?.isMuted == true) {
      Icon(painterResource(R.drawable.ic_notifications_off), contentDescription = stringResource(R.string.notifications_conversation))
    } else {
      Icon(Icons.Filled.Notifications, contentDescription = stringResource(R.string.notifications_conversation))
    }
  }
  IconButton(onClick = { isOpen = true }) {
    Icon(Icons.Filled.MoreVert, contentDescription = stringResource(R.string.more))
  }
  DropdownMenu(expanded = isOpen, onDismissRequest = { isOpen = false }) {
    DropdownMenuItem(
      text = { Text(stringResource(if (isFavorite) R.string.favorite_remove else R.string.favorite_add)) },
      leadingIcon = { Icon(Icons.Filled.Star, contentDescription = null) },
      onClick = {
        isOpen = false
        viewModel.act(ChatActions.setFavorite(conversationId, !isFavorite))
      },
    )
    DropdownMenuItem(text = { Text(stringResource(R.string.files_title)) }, onClick = {
      isOpen = false
      onOpenFiles()
    })
    DropdownMenuItem(text = { Text(stringResource(R.string.channel_bookmarks)) }, onClick = {
      isOpen = false
      isShowingBookmarks = true
    })
    if (canArchive) {
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

@Composable
private fun PinnedBanner(message: Message) {
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Row(Modifier.padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
      Icon(
        painterResource(R.drawable.ic_push_pin),
        contentDescription = stringResource(R.string.message_pinned),
        modifier = Modifier.padding(end = 8.dp).size(16.dp),
      )
      Text(message.body, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodySmall)
    }
  }
}

/** The feed plus the long-press action sheet of the selected message. */
@Composable
private fun MessageList(
  viewModel: MainViewModel,
  conversationId: String,
  snapshot: ChatSnapshot?,
  messages: List<Message>,
  extras: Map<String, MessageExtras>,
  firstUnread: FirstUnread,
  sentCount: Int,
  onOpenThread: ((String) -> Unit)?,
  maybeOnReply: ((Message) -> Unit)?,
  rights: ConversationRights,
  modifier: Modifier,
  maybeFocusMessage: String?,
  onOpenPermalink: (String) -> Unit,
  older: OlderMessages = OlderMessages.None,
) {
  var maybeSelected by remember { mutableStateOf<Message?>(null) }
  MessageFeed(
    viewModel = viewModel,
    snapshot = snapshot,
    messages = messages,
    extras = extras,
    firstUnread = firstUnread,
    sentCount = sentCount,
    onOpenThread = onOpenThread,
    maybeOnReply = maybeOnReply,
    onLongPress = { maybeSelected = it },
    modifier = modifier,
    maybeFocusMessage = maybeFocusMessage,
    onOpenPermalink = onOpenPermalink,
    canReact = rights.canReact,
    older = older,
  )
  maybeSelected?.let { message ->
    MessageActions(
      viewModel = viewModel,
      conversationId = conversationId,
      message = message,
      maybeExtras = extras[message.id],
      snapshot = snapshot,
      rights = rights,
      onOpenThread = onOpenThread,
      onDismiss = { maybeSelected = null },
    )
  }
}

/** Opens a decrypted download in another app through the FileProvider. */
fun openFile(context: Context, file: File, mediaType: String) {
  val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", file)
  val intent = Intent(Intent.ACTION_VIEW).setDataAndType(uri, mediaType).addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
  runCatching { context.startActivity(Intent.createChooser(intent, file.name)) }
}

@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
private fun MessageActions(
  viewModel: MainViewModel,
  conversationId: String,
  message: Message,
  maybeExtras: MessageExtras?,
  snapshot: ChatSnapshot?,
  rights: ConversationRights,
  onOpenThread: ((String) -> Unit)?,
  onDismiss: () -> Unit,
) {
  val context = LocalContext.current
  val scope = rememberCoroutineScope()
  val isMine = message.author == snapshot?.me?.id
  var isEditing by remember { mutableStateOf(false) }
  var isForwarding by remember { mutableStateOf(false) }
  var isConfirmingDelete by remember { mutableStateOf(false) }
  val actions = messageActions(
    rights = rights,
    isMine = isMine,
    isDeleted = maybeExtras?.isDeleted == true,
    hasThreads = onOpenThread != null,
    wantsAcknowledgement = maybeExtras?.wantsAcknowledgement == true,
  )
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp)) {
      // One row, spread over the width: the reactions are the sheet's most used part.
      if (rights.canReact) {
        QuickReactions { emoji ->
          viewModel.act(ChatActions.react(conversationId, message.id, emoji))
          onDismiss()
        }
      }
      for (action in actions) {
        when (action) {
          MessageAction.Reply -> SheetItem(R.string.action_reply) {
            ReplyTarget.set(conversationId, message, snapshot?.name(message.author).orEmpty())
            onDismiss()
          }
          MessageAction.ReplyInThread -> SheetItem(R.string.reply_in_thread) {
            onOpenThread?.invoke(message.id)
            onDismiss()
          }
          MessageAction.Edit -> SheetItem(R.string.action_edit) { isEditing = true }
          MessageAction.Delete -> SheetItem(R.string.action_delete) { isConfirmingDelete = true }
          MessageAction.Forward -> SheetItem(R.string.action_forward) { isForwarding = true }
          MessageAction.Pin -> SheetItem(if (maybeExtras?.isPinned == true) R.string.action_unpin else R.string.action_pin) {
            viewModel.act(ChatActions.setPinned(conversationId, message.id, maybeExtras?.isPinned != true))
            onDismiss()
          }
          MessageAction.Save -> SheetItem(if (maybeExtras?.isSaved == true) R.string.action_unsave else R.string.action_save) {
            viewModel.act(ChatActions.setSaved(conversationId, message.id, maybeExtras?.isSaved != true))
            onDismiss()
          }
          MessageAction.Acknowledge -> SheetItem(R.string.action_acknowledge) {
            viewModel.act(ChatActions.acknowledge(conversationId, message.id))
            onDismiss()
          }
          MessageAction.CopyLink -> SheetItem(R.string.action_copy_link) {
            scope.launch {
              viewModel.readChat { query(ChatQueries.permalink(conversationId, message.id)).maybeLink }?.let { link ->
                val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                clipboard.setPrimaryClip(sensitiveClip(link))
              }
              onDismiss()
            }
          }
          MessageAction.CopyText -> SheetItem(R.string.action_copy_text) {
            val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            clipboard.setPrimaryClip(sensitiveClip(message.body))
            onDismiss()
          }
          MessageAction.MarkUnread -> SheetItem(R.string.action_mark_unread) {
            viewModel.act(ChatActions.markUnread(conversationId, message.id))
            onDismiss()
          }
        }
      }
      if (!maybeExtras?.edits.isNullOrEmpty()) {
        Text(
          stringResource(R.string.edit_history),
          style = MaterialTheme.typography.titleSmall,
          modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
        )
        maybeExtras.edits.forEach { edit ->
          Text(edit.body, style = MaterialTheme.typography.bodySmall, modifier = Modifier.padding(horizontal = 16.dp))
        }
      }
    }
  }
  if (isConfirmingDelete) {
    AlertDialog(
      onDismissRequest = { isConfirmingDelete = false },
      title = { Text(stringResource(R.string.delete_confirm_title)) },
      text = { Text(stringResource(R.string.delete_confirm_text)) },
      confirmButton = {
        TextButton(onClick = {
          viewModel.act(ChatActions.delete(conversationId, message.id))
          onDismiss()
        }) { Text(stringResource(R.string.action_delete), color = MaterialTheme.colorScheme.error) }
      },
      dismissButton = { TextButton(onClick = { isConfirmingDelete = false }) { Text(stringResource(R.string.cancel)) } },
    )
  }
  if (isEditing) {
    TextDialog(
      title = R.string.action_edit,
      label = R.string.message_edit_label,
      whenEmpty = R.string.message_edit_empty,
      initial = message.body,
      onConfirm = { body ->
        viewModel.tryAct(ChatActions.edit(conversationId, message.id, body)).also { if (it == null) onDismiss() }
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

/** The quick reactions at the top of a message's action sheet, spread over the width. */
@Composable
private fun QuickReactions(onReact: (String) -> Unit) {
  Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
    for (emoji in QUICK_REACTIONS) {
      val label = stringResource(R.string.react_quick, emoji)
      TextButton(
        onClick = { onReact(emoji) },
        contentPadding = PaddingValues(4.dp),
        // "React with 👍": TalkBack names the emoji and says what the button does.
        modifier = Modifier.weight(1f).semantics(mergeDescendants = true) { contentDescription = label },
      ) { Text(emoji, style = MaterialTheme.typography.titleLarge) }
    }
  }
}

@Composable
private fun SheetItem(label: Int, onClick: () -> Unit) {
  Text(
    stringResource(label),
    modifier = Modifier.fillMaxWidth().clickable(onClick = onClick).padding(horizontal = 24.dp, vertical = 14.dp),
  )
}

/**
 * A dialog that asks for one text (a channel name, an edited message). An empty text and the
 * server's refusal ([onConfirm] returns its message, null on success) are shown at the field; the
 * dialog stays open with the input until the change went through.
 */
@Composable
fun TextDialog(
  @StringRes title: Int,
  @StringRes label: Int,
  @StringRes whenEmpty: Int,
  initial: String,
  onConfirm: suspend (String) -> Int?,
  onDismiss: () -> Unit,
) {
  val scope = rememberCoroutineScope()
  var text by remember { mutableStateOf(initial) }
  var hasTried by remember { mutableStateOf(false) }
  var isSaving by remember { mutableStateOf(false) }
  var maybeFailure by remember { mutableStateOf<Int?>(null) }
  val focus = remember { FocusRequester() }
  val maybeError = requiredTextError(text, whenEmpty).takeIf { hasTried } ?: maybeFailure
  val confirm: () -> Unit = {
    hasTried = true
    if (text.isBlank()) {
      focus.requestFocus()
    } else if (!isSaving) {
      isSaving = true
      scope.launch {
        maybeFailure = onConfirm(text)
        if (maybeFailure != null) focus.requestFocus()
        isSaving = false
      }
    }
  }
  LaunchedEffect(Unit) { focus.requestFocus() }
  AlertDialog(
    onDismissRequest = onDismiss,
    title = { Text(stringResource(title)) },
    text = {
      OutlinedTextField(
        value = text,
        onValueChange = {
          text = it
          maybeFailure = null
        },
        label = { Text(stringResource(label)) },
        isError = maybeError != null,
        supportingText = supportingText(maybeError),
        modifier = Modifier.fillMaxWidth().focusRequester(focus).fieldError(maybeError),
      )
    },
    confirmButton = { TextButton(onClick = confirm, enabled = !isSaving) { Text(stringResource(R.string.ok)) } },
    dismissButton = { TextButton(onClick = onDismiss) { Text(stringResource(R.string.cancel)) } },
  )
}

@Composable
internal fun ForwardDialog(snapshot: ChatSnapshot, onPick: (String) -> Unit, onDismiss: () -> Unit) {
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

/** The message field with its suggestions, reply line and attachments (internal: screenshots show it with sample data). */
@OptIn(ExperimentalLayoutApi::class)
@Composable
internal fun Composer(
  viewModel: MainViewModel,
  conversationId: String,
  maybeThread: String?,
  snapshot: ChatSnapshot?,
  onSent: () -> Unit = {},
) {
  val context = LocalContext.current
  var text by rememberSaveable(conversationId, maybeThread) { mutableStateOf("") }
  var isDraftLoaded by remember(conversationId, maybeThread) { mutableStateOf(false) }
  // Kept in the view model: picked files survive looking at another conversation (until sent).
  var files by viewModel.pickedFiles.entry("$conversationId/${maybeThread.orEmpty()}", emptyList())
  var priority by rememberSaveable(conversationId) { mutableStateOf("normal") }
  var wantsAcknowledgement by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isSilent by rememberSaveable(conversationId) { mutableStateOf(false) }
  var alsoToChannel by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isOptionsOpen by remember { mutableStateOf(false) }
  val replyTarget by ReplyTarget.current.collectAsState()
  val maybeReply = replyTarget?.takeIf { it.conversation == conversationId && maybeThread == null }
  val pickers = rememberAttachmentPickers(viewModel) { picked -> files = files + picked }
  val focus = remember { FocusRequester() }
  val isSendWithEnter = remember { ComposerPrefs.prefs(context).isSendWithEnter }
  val isSharingTyping = remember { ComposerPrefs.prefs(context).isSharingTyping }
  var maybeLastTypingAt by remember { mutableStateOf<Long?>(null) }
  LaunchedEffect(maybeReply) {
    // Replying starts typing right away.
    if (maybeReply != null) focus.requestFocus()
  }
  LaunchedEffect(conversationId, maybeThread) {
    // A draft from this or another device of the user.
    if (text.isEmpty()) text = viewModel.readChat { query(ChatQueries.draft(conversationId, maybeThread)).maybeBody }.orEmpty()
    isDraftLoaded = true
  }
  val maybePendingShare by PendingShare.current.collectAsState()
  LaunchedEffect(maybePendingShare, isDraftLoaded) {
    // Text and files shared from another app land in the composer; the user sends them.
    val share = maybePendingShare?.takeIf { it.conversation == conversationId && maybeThread == null } ?: return@LaunchedEffect
    if (!isDraftLoaded) return@LaunchedEffect
    val imported = share.content.uris.mapNotNull { viewModel.importFile(it) }
    text = listOf(text, share.content.text).filter { it.isNotBlank() }.joinToString("\n")
    files = files + imported
    // Cleared last: clearing restarts this effect, which would cancel the import.
    PendingShare.current.value = null
    focus.requestFocus()
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
    // `:)` and `:tada:` become emoji, as on desktop.
    viewModel.act(ChatActions.send(conversationId, emojiConvert(text.trim()), options))
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, ""))
    text = ""
    files = emptyList()
    priority = "normal"
    wantsAcknowledgement = false
    isSilent = false
    ReplyTarget.current.value = null
    onSent()
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
  val canSend = text.isNotBlank() || files.isNotEmpty()
  fun trySend() {
    val confirmOver = viewModel.confirmChannelOver().toUInt()
    if (needsChannelConfirmation(text, members.size.toUInt(), confirmOver)) isConfirmingChannel = true else send()
  }
  Column(Modifier.fillMaxWidth()) {
    EmojiSuggestions(text) { emoji -> text = EmojiInput.complete(text, emoji) }
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
          val removeLabel = stringResource(R.string.composer_remove_file, file.fileName)
          InputChip(
            selected = false,
            onClick = {
              files = files - file
              viewModel.discardFile(file)
            },
            label = { Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis) },
            leadingIcon = { Icon(painterResource(R.drawable.ic_attach_file), contentDescription = null, modifier = Modifier.size(18.dp)) },
            trailingIcon = { Icon(Icons.Filled.Close, contentDescription = removeLabel, modifier = Modifier.size(18.dp)) },
            modifier = Modifier.widthIn(max = 240.dp),
          )
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
    ComposerField(
      text = text,
      onTextChange = {
        text = it
        val now = System.currentTimeMillis()
        if (isSharingTyping && shouldNotifyTyping(it, now, maybeLastTypingAt)) {
          maybeLastTypingAt = now
          viewModel.act(ChatActions.typing(conversationId))
        }
      },
      canSend = canSend,
      isSendWithEnter = isSendWithEnter,
      focus = focus,
      onAttach = { isOptionsOpen = true },
      onCamera = pickers.takePhoto,
      onSend = ::trySend,
    )
  }
  if (isOptionsOpen) {
    AttachSheet(pickers, onDismiss = { isOptionsOpen = false }) {
      SendOptionsSection(
        priority = priority,
        onPriority = { priority = it },
        wantsAcknowledgement = wantsAcknowledgement,
        onAcknowledgement = { wantsAcknowledgement = it },
        isSilent = isSilent,
        onSilent = { isSilent = it },
        maybeAlsoToChannel = alsoToChannel.takeIf { maybeThread != null },
        onAlsoToChannel = { alsoToChannel = it },
      )
    }
  }
}

/**
 * The message field as in the common messengers (#315, #704): a rounded pill with attach on the
 * left and the camera on the right inside it, and a round accent send button beside it, disabled
 * while there is nothing to send. Voice messages don't exist yet, so the button is always "send".
 */
@Composable
private fun ComposerField(
  text: String,
  onTextChange: (String) -> Unit,
  canSend: Boolean,
  isSendWithEnter: Boolean,
  focus: FocusRequester,
  onAttach: () -> Unit,
  onCamera: () -> Unit,
  onSend: () -> Unit,
) {
  Row(
    Modifier.fillMaxWidth().padding(start = 8.dp, end = 8.dp, top = 4.dp, bottom = 6.dp),
    verticalAlignment = Alignment.Bottom,
  ) {
    Surface(
      shape = RoundedCornerShape(COMPOSER_CORNER),
      color = MaterialTheme.colorScheme.surfaceContainerHigh,
      modifier = Modifier.weight(1f),
    ) {
      Row(Modifier.padding(horizontal = 4.dp), verticalAlignment = Alignment.Bottom) {
        IconButton(onClick = onAttach, modifier = Modifier.padding(vertical = 4.dp)) {
          Icon(painterResource(R.drawable.ic_attach_file), contentDescription = stringResource(R.string.attach))
        }
        TextField(
          value = text,
          onValueChange = onTextChange,
          placeholder = { Text(stringResource(R.string.composer_hint), style = MaterialTheme.typography.bodyLarge) },
          textStyle = MaterialTheme.typography.bodyLarge,
          modifier = Modifier
            .weight(1f)
            .focusRequester(focus)
            .onPreviewKeyEvent { event ->
              // Hardware keyboards: Enter sends when the user chose so; Shift+Enter is a new line.
              val isSendKey = isSendWithEnter && event.type == KeyEventType.KeyDown && event.key == Key.Enter && !event.isShiftPressed
              if (isSendKey && canSend) onSend()
              isSendKey
            },
          maxLines = 6,
          colors = TextFieldDefaults.colors(
            focusedContainerColor = Color.Transparent,
            unfocusedContainerColor = Color.Transparent,
            disabledContainerColor = Color.Transparent,
            focusedIndicatorColor = Color.Transparent,
            unfocusedIndicatorColor = Color.Transparent,
            disabledIndicatorColor = Color.Transparent,
          ),
          keyboardOptions = KeyboardOptions(
            capitalization = KeyboardCapitalization.Sentences,
            imeAction = if (isSendWithEnter) ImeAction.Send else ImeAction.Default,
          ),
          keyboardActions = KeyboardActions(onSend = { if (canSend) onSend() }),
        )
        // While typing the field needs the room; the camera comes back when it is empty.
        if (text.isEmpty()) {
          IconButton(onClick = onCamera, modifier = Modifier.padding(vertical = 4.dp)) {
            Icon(painterResource(R.drawable.ic_photo_camera), contentDescription = stringResource(R.string.attach_camera))
          }
        }
      }
    }
    FilledIconButton(
      onClick = onSend,
      enabled = canSend,
      shape = CircleShape,
      colors = IconButtonDefaults.filledIconButtonColors(
        disabledContainerColor = MaterialTheme.colorScheme.surfaceContainerHigh,
        disabledContentColor = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.6f),
      ),
      modifier = Modifier.padding(start = 6.dp, bottom = 4.dp).size(SEND_BUTTON_SIZE),
    ) {
      Icon(Icons.AutoMirrored.Filled.Send, contentDescription = stringResource(R.string.send))
    }
  }
}

/**
 * Emoji for the `:name` being typed, from the core's catalog (all of Unicode); a tap puts the
 * emoji in. Emoji newer than the device's font are left out: they would show as empty boxes.
 */
@Composable
private fun EmojiSuggestions(text: String, onPick: (String) -> Unit) {
  val maybeQuery = EmojiInput.activeQuery(text) ?: return
  val suggestions = remember(maybeQuery) {
    val paint = Paint()
    emojiSuggestions(maybeQuery, (MAX_EMOJI_SUGGESTIONS * 4).toUInt())
      .filter { PaintCompat.hasGlyph(paint, it.emoji) }
      .take(MAX_EMOJI_SUGGESTIONS)
  }
  if (suggestions.isEmpty()) return
  LazyRow(
    Modifier.fillMaxWidth().padding(horizontal = 8.dp),
    horizontalArrangement = Arrangement.spacedBy(6.dp),
  ) {
    items(suggestions, key = { it.emoji }) { suggestion ->
      AssistChip(
        onClick = { onPick(suggestion.emoji) },
        label = { Text("${suggestion.emoji} :${suggestion.shortcode}:") },
      )
    }
  }
}

/** Priority, read confirmation, silent and "also send to channel", in the composer's "+" sheet. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun SendOptionsSection(
  priority: String,
  onPriority: (String) -> Unit,
  wantsAcknowledgement: Boolean,
  onAcknowledgement: (Boolean) -> Unit,
  isSilent: Boolean,
  onSilent: (Boolean) -> Unit,
  maybeAlsoToChannel: Boolean?,
  onAlsoToChannel: (Boolean) -> Unit,
) {
  HorizontalDivider(Modifier.padding(vertical = 8.dp))
  Text(
    stringResource(R.string.send_options),
    style = MaterialTheme.typography.titleSmall,
    modifier = Modifier.padding(horizontal = 24.dp, vertical = 4.dp),
  )
  FlowRow(Modifier.padding(horizontal = 24.dp), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    for ((value, label) in PRIORITIES) {
      FilterChip(selected = priority == value, onClick = { onPriority(value) }, label = { Text(stringResource(label)) })
    }
  }
  OptionSwitch(R.string.option_acknowledge, wantsAcknowledgement, onAcknowledgement)
  OptionSwitch(R.string.option_silent, isSilent, onSilent)
  maybeAlsoToChannel?.let { OptionSwitch(R.string.option_also_to_channel, it, onAlsoToChannel) }
}

@Composable
private fun OptionSwitch(label: Int, isOn: Boolean, onChange: (Boolean) -> Unit) {
  Row(
    Modifier.fillMaxWidth().toggleable(value = isOn, onValueChange = onChange).padding(horizontal = 24.dp, vertical = 8.dp),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Text(stringResource(label), modifier = Modifier.weight(1f))
    Switch(checked = isOn, onCheckedChange = null)
  }
}

private val PRIORITIES = listOf(
  "normal" to R.string.priority_normal,
  "important" to R.string.priority_important,
  "urgent" to R.string.priority_urgent,
)

/** Before a call: join with the camera on or muted. */
@Composable
internal fun PreJoinDialog(name: String, onDismiss: () -> Unit, onJoin: (CallOptions) -> Unit) {
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

/** The composer pill's corner radius: fully round at one line, a soft box when it grows. */
private val COMPOSER_CORNER = 28.dp

/** The round send button beside the composer pill. */
private val SEND_BUTTON_SIZE = 48.dp

/** Emoji offered for a typed `:name`. */
private const val MAX_EMOJI_SUGGESTIONS = 8

/** How often the typing line is re-read while someone types (indicators last 6 s). */
private const val TYPING_POLL_MS = 2000L

/** How often a running upload or download is re-read. */
private const val TRANSFER_POLL_MS = 1000L

/** Drafts are saved (and synced to the user's other devices) after typing pauses this long. */
private const val DRAFT_SAVE_DELAY_MS = 800L
