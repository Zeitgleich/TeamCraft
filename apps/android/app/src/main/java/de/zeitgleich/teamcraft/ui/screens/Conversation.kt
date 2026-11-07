package de.zeitgleich.teamcraft.ui.screens

import android.widget.Toast
import androidx.annotation.StringRes
import androidx.compose.foundation.layout.Box
import androidx.compose.ui.draw.clip
import androidx.compose.ui.layout.ContentScale
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
import androidx.compose.foundation.Image
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.foundation.layout.offset
import androidx.compose.material.icons.filled.Edit
import de.zeitgleich.teamcraft.ui.PictureEdits
import de.zeitgleich.teamcraft.ui.Pictures
import androidx.compose.material3.Button
import uniffi.teamcraft_ffi.CallPhase
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.disabled
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
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
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import java.io.File
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.ui.PictureTarget
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
import de.zeitgleich.teamcraft.core.UnseenReactions
import de.zeitgleich.teamcraft.core.VoiceMessagePolicy
import de.zeitgleich.teamcraft.ui.ServerHistory
import de.zeitgleich.teamcraft.ui.olderState
import de.zeitgleich.teamcraft.ui.ComposerPrefs
import de.zeitgleich.teamcraft.ui.ConversationRights
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.media.MediaDraftKey
import de.zeitgleich.teamcraft.ui.media.MediaFlow
import de.zeitgleich.teamcraft.ui.media.MediaFlowLaunch
import de.zeitgleich.teamcraft.ui.MessageAction
import de.zeitgleich.teamcraft.ui.VoiceOwner
import de.zeitgleich.teamcraft.ui.messageActions
import de.zeitgleich.teamcraft.ui.EmojiInput
import de.zeitgleich.teamcraft.ui.FeedSeed
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
import uniffi.teamcraft_ffi.LeaveChoice
import uniffi.teamcraft_ffi.ConnectionState
import uniffi.teamcraft_ffi.JoinRequestItem
import uniffi.teamcraft_ffi.KeyCatchUpItem
import uniffi.teamcraft_ffi.NotifiedConversation
import uniffi.teamcraft_ffi.emojiConvert
import uniffi.teamcraft_ffi.emojiSuggestions
import uniffi.teamcraft_ffi.needsChannelConfirmation
import android.os.Build

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
  /** Opens another conversation: the room a call moved into (issue #1290). */
  onOpenConversation: (String) -> Unit = {},
) {
  val changes by viewModel.changeCount.collectAsState()
  var snapshot by remember { mutableStateOf(viewModel.chat.maybeKeptSnapshot()) }
  // What it showed last or was read ahead with: the messages are there in the first frame.
  val maybeSeed = remember(conversationId) { viewModel.feedSeed(conversationId) }
  var messages by remember(conversationId) { mutableStateOf(maybeSeed?.messages ?: emptyList()) }
  var extras by remember(conversationId) { mutableStateOf(maybeSeed?.extras ?: emptyMap()) }
  var pinned by remember(conversationId) { mutableStateOf<List<Message>>(emptyList()) }
  // Admins asking to join this channel, when the user owns it (admin.md §2.4.1).
  var joinRequests by remember { mutableStateOf<List<JoinRequestItem>>(emptyList()) }
  var firstUnread by remember(conversationId) { mutableStateOf(maybeSeed?.firstUnread ?: FirstUnread.Loading) }
  // Others' reactions to the user's messages not seen yet: the heart button (issue #1009).
  var unseenReactions by remember(conversationId) { mutableStateOf(UnseenReactions()) }
  // Counts sends from this screen's composer: each one brings the newest message into view.
  var sentCount by remember { mutableIntStateOf(0) }
  var maybeKeyCatchUp by remember { mutableStateOf<KeyCatchUpItem?>(null) }
  // People in the call running here now; 0 without one (issue #1352).
  var runningCallCount by remember(conversationId) { mutableIntStateOf(0) }
  // What the notification that opened it showed, until the store has it (issue #1065).
  var maybeNotified by remember(conversationId) { mutableStateOf<NotifiedConversation?>(null) }
  val feed = remember(conversationId) {
    PagedFeed(PAGE, PAGE_BACK) { maybeOldest, older -> viewModel.readChat { tail(conversationId, maybeOldest, older) } }
  }
  var hasOlder by remember(conversationId) { mutableStateOf(maybeSeed?.hasOlder ?: false) }
  var isLoadingOlder by remember(conversationId) { mutableStateOf(false) }
  // Older history the store doesn't hold: read when the feed reaches the store's end.
  var serverHistory by remember(conversationId) { mutableStateOf(ServerHistory.COMPLETE) }
  var isAskingServer by remember(conversationId) { mutableStateOf(false) }
  val scope = rememberCoroutineScope()
  val rights = rememberRights(viewModel, conversationId, changes)
  LaunchedEffect(conversationId) {
    // Where the user stopped reading, before marking the conversation read.
    val maybeMessage = viewModel.readChat { query(ChatQueries.firstUnread(conversationId)).maybeMessage }
    firstUnread = maybeMessage?.let { FirstUnread.At(it) } ?: FirstUnread.None
  }
  LaunchedEffect(conversationId) {
    // One pass after another, each with the newest state: while syncing, changes come faster than a
    // pass, and restarting on each would keep cancelling the reads behind the messages. The
    // messages come first; the snapshot (names, the header) is shared with the other screens.
    viewModel.changeCount.collect {
      val maybeTail = feed.reread()
      messages = maybeTail?.messages ?: emptyList()
      hasOlder = maybeTail?.hasOlder ?: false
      // Reactions, files and receipts belong to the bubbles: right after them.
      extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: extras
      // Read now: opened again, it starts at the end without an unread line.
      viewModel.keepFeedSeed(conversationId, FeedSeed(messages, hasOlder, extras, FirstUnread.None))
      snapshot = viewModel.readChat { snapshot() } ?: snapshot
      serverHistory = viewModel.readChat { query(ChatQueries.olderHistory(conversationId)).olderHistory } ?: serverHistory
      isAskingServer = false
      maybeKeyCatchUp = viewModel.keyCatchUp()
      // After the messages: once the store has the notified one, the core no longer names it.
      maybeNotified = viewModel.notifiedConversation(conversationId)
      pinned = viewModel.readChat { query(ChatQueries.pinned(conversationId)).messages } ?: pinned
      unseenReactions = viewModel.readChat { query(ChatQueries.unseenReactions(conversationId)).unseen } ?: unseenReactions
      joinRequests = requestsIn(conversationId, viewModel.joinRequests.list())
      runningCallCount = viewModel.runningCalls()[conversationId] ?: 0
    }
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
  // Deleted or left elsewhere while open: back to the list, not a screen that keeps loading.
  var wasKnown by remember(conversationId) { mutableStateOf(false) }
  LaunchedEffect(conversationId, conversation != null, snapshot != null) {
    if (conversation != null) {
      wasKnown = true
    } else if (wasKnown && snapshot != null) {
      onBack()
    }
  }
  ChatScaffold(
    viewModel = viewModel,
    topBar = {
      ConversationTopBar(snapshot, conversation, maybeNotified, onBack) {
        ConversationMenu(viewModel, conversationId, conversation, rights, onOpenFiles)
      }
    },
  ) {
    SyncingLine(isCatchingUp = conversation?.isCatchingUp == true)
    RunningCallBanner(viewModel, conversationId, conversation?.name.orEmpty(), runningCallCount)
    if (pinned.isNotEmpty()) PinnedBanner(pinned.last())
    JoinRequestBanner(joinRequests, viewModel.joinRequests)
    // Messages here wait for their keys (a newly linked device, issue #888): they show as
    // placeholders in the list; this says how far the waiting conversations have caught up.
    val keyCatchUp = maybeKeyCatchUp
    if (keyCatchUp != null && messages.any { it.maybePlaceholder == "waiting" }) {
      val total = keyCatchUp.total.toInt()
      Text(
        pluralStringResource(R.plurals.key_catch_up_progress, total, keyCatchUp.caughtUp.toInt(), total),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
      )
    }
    MessageList(
      viewModel = viewModel,
      conversationId = conversationId,
      snapshot = snapshot,
      messages = withNotified(messages, maybeNotified),
      extras = extras,
      firstUnread = firstUnread,
      sentCount = sentCount,
      onOpenThread = onOpenThread,
      maybeOnReply = { message: Message ->
        ReplyTarget.set(conversationId, message, snapshot?.name(message.author).orEmpty())
      }.takeIf { rights.canPost },
      maybeOnReplyImage = { file: FileInfo ->
        ReplyTarget.setImage(file, file.maybeSender?.let { snapshot?.name(it) }.orEmpty())
      }.takeIf { rights.canPost },
      rights = rights,
      modifier = Modifier.weight(1f),
      maybeFocusMessage = maybeFocusMessage,
      onOpenPermalink = onOpenPermalink,
      onOpenConversation = onOpenConversation,
      unseenReactions = unseenReactions,
      onReactionsSeen = { seen -> viewModel.act(ChatActions.markReactionsSeen(conversationId, seen)) },
      older = OlderMessages(
        olderState(hasOlder, isLoadingOlder || isAskingServer, serverHistory),
        onRetry = { viewModel.act(ChatActions.loadOlderHistory(conversationId)) },
      ) {
        if (isLoadingOlder || isAskingServer) return@OlderMessages
        if (!hasOlder) {
          // The core answers in the store: the change that follows says how it went.
          isAskingServer = true
          viewModel.act(ChatActions.loadOlderHistory(conversationId))
          return@OlderMessages
        }
        isLoadingOlder = true
        scope.launch {
          // A failed read leaves the rows as they are; the next scroll near the top asks again.
          feed.older()?.let { tail ->
            messages = tail.messages
            hasOlder = tail.hasOlder
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
      ReadOnlyNote(isArchived = conversation?.isArchived == true)
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
  var snapshot by remember { mutableStateOf(viewModel.chat.maybeKeptSnapshot()) }
  var messages by remember { mutableStateOf<List<Message>>(emptyList()) }
  var extras by remember { mutableStateOf<Map<String, MessageExtras>>(emptyMap()) }
  var isFollowed by remember { mutableStateOf(false) }
  var sentCount by remember { mutableIntStateOf(0) }
  val rights = rememberRights(viewModel, conversationId, changes)
  LaunchedEffect(threadId) {
    // As in the conversation: one pass after another, the messages first.
    viewModel.changeCount.collect {
      messages = viewModel.readChat { query(ChatQueries.thread(conversationId, threadId)).messages } ?: messages
      snapshot = viewModel.readChat { snapshot() } ?: snapshot
      extras = viewModel.readChat { extras(conversationId, messages.map { it.id }) } ?: extras
      isFollowed = viewModel.readChat { query(ChatQueries.isThreadFollowed(conversationId, threadId)).value } ?: isFollowed
    }
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
private fun ReadOnlyNote(isArchived: Boolean = false) {
  Text(
    stringResource(if (isArchived) R.string.read_only_archived else R.string.read_only_channel),
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
  val canDelete = maybeConversation?.isDirect == false && rights.canDeleteChannel
  var leave by remember(conversationId) { mutableStateOf(LeaveChoice.HIDDEN) }
  var isOpen by remember { mutableStateOf(false) }
  LaunchedEffect(conversationId, isOpen) { leave = viewModel.leaveChoice(conversationId) }
  var isNotifying by remember { mutableStateOf(false) }
  var isShowingBookmarks by remember { mutableStateOf(false) }
  // A channel's members with their roles (#1262); DMs and group DMs have no roles.
  val isChannel = maybeConversation?.kind == "Public" || maybeConversation?.kind == "Private"
  var isShowingMembers by remember { mutableStateOf(false) }
  var maybeConfirming by remember { mutableStateOf<ChannelConfirmation?>(null) }
  var isPreparingCall by remember { mutableStateOf(false) }
  IconButton(onClick = { isPreparingCall = true }) {
    Icon(Icons.Filled.Call, contentDescription = stringResource(R.string.call_start))
  }
  if (isPreparingCall) {
    val isNotetakerInCall by produceState(false, conversationId) {
      value = viewModel.isNotetakerInRunningCall(conversationId)
    }
    val runningCount by produceState(0, conversationId) { value = viewModel.runningCalls()[conversationId] ?: 0 }
    PreJoinDialog(name, isNotetakerInCall, runningCount, onDismiss = { isPreparingCall = false }) { options ->
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
    if (isChannel) {
      DropdownMenuItem(text = { Text(stringResource(R.string.members_title)) }, onClick = {
        isOpen = false
        isShowingMembers = true
      })
    }
    if (canArchive) {
      DropdownMenuItem(text = { Text(stringResource(R.string.channel_archive)) }, onClick = {
        isOpen = false
        maybeConfirming = ChannelConfirmation.Archive
      })
    }
    // The core's rule, as in the list's menu: the last admin of an active channel is told why not.
    when (leave) {
      LeaveChoice.HIDDEN -> Unit
      LeaveChoice.LAST_ADMIN -> DropdownMenuItem(
        text = { Text(stringResource(if (canDelete) R.string.entry_leave_last_channel_admin_or_delete else R.string.entry_leave_last_channel_admin)) },
        enabled = false,
        onClick = {},
      )
      LeaveChoice.ALLOWED, LeaveChoice.LAST_MEMBER -> DropdownMenuItem(text = { Text(stringResource(R.string.leave_channel)) }, onClick = {
        isOpen = false
        maybeConfirming = ChannelConfirmation.Leave
      })
    }
    if (canDelete) {
      DropdownMenuItem(text = { Text(stringResource(R.string.channel_delete)) }, onClick = {
        isOpen = false
        maybeConfirming = ChannelConfirmation.Delete
      })
    }
  }
  if (isNotifying) ConversationNotificationsDialog(viewModel, conversationId) { isNotifying = false }
  if (isShowingMembers) {
    ChannelMembersSheet(viewModel, conversationId, onDismiss = { isShowingMembers = false })
  }
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
      leave = leave,
      onConfirm = {
        when (confirmation) {
          ChannelConfirmation.Archive -> viewModel.act(ChatActions.archiveChannel(conversationId))
          ChannelConfirmation.Leave -> viewModel.act(ChatActions.leaveChannel(conversationId, leave == LeaveChoice.LAST_MEMBER))
          ChannelConfirmation.Delete -> viewModel.act(ChatActions.deleteChannel(conversationId))
        }
      },
      onDismiss = { maybeConfirming = null },
    )
  }
}

/** A channel menu action that asks before it runs. */
private enum class ChannelConfirmation { Archive, Leave, Delete }

@Composable
private fun ChannelConfirmationDialog(
  confirmation: ChannelConfirmation,
  name: String,
  leave: LeaveChoice,
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
      stringResource(leaveText(leave)),
      stringResource(R.string.channel_leave_confirm),
    )
    ChannelConfirmation.Delete -> Triple(
      stringResource(R.string.channel_delete_title, name),
      stringResource(R.string.channel_delete_text),
      stringResource(R.string.channel_delete_confirm),
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
  maybeOnReplyImage: ((FileInfo) -> Unit)? = null,
  rights: ConversationRights,
  modifier: Modifier,
  maybeFocusMessage: String?,
  onOpenPermalink: (String) -> Unit,
  onOpenConversation: (String) -> Unit = {},
  older: OlderMessages = OlderMessages.None,
  unseenReactions: UnseenReactions = UnseenReactions(),
  onReactionsSeen: (List<String>) -> Unit = {},
) {
  var maybeSelected by remember { mutableStateOf<Message?>(null) }
  var maybeSelectedImage by remember { mutableStateOf<FileInfo?>(null) }
  val imageScope = rememberCoroutineScope()
  val imageContext = LocalContext.current
  var maybeViewingFromSheet by remember { mutableStateOf<PictureTarget?>(null) }
  maybeSelectedImage?.let { image ->
    ImageActions(
      viewModel = viewModel,
      snapshot = snapshot,
      image = image,
      maybeOnReply = maybeOnReplyImage,
      // Goes on after the sheet closed: it may have to download the picture first.
      onSave = { imageScope.launch { downloadAndSaveToGallery(viewModel, imageContext, image) } },
      onOpen = { maybeViewingFromSheet = PictureTarget.of(image) },
      onDismiss = { maybeSelectedImage = null },
    )
  }
  MessageFeed(
    viewModel = viewModel,
    snapshot = snapshot,
    messages = messages,
    extras = extras,
    firstUnread = firstUnread,
    sentCount = sentCount,
    onOpenThread = onOpenThread,
    maybeOnReply = maybeOnReply,
    maybeOnReplyImage = maybeOnReplyImage,
    onLongPress = { maybeSelected = it },
    onLongPressImage = { maybeSelectedImage = it },
    maybeOpenPicture = maybeViewingFromSheet,
    onPictureOpened = { maybeViewingFromSheet = null },
    modifier = modifier,
    maybeFocusMessage = maybeFocusMessage,
    onOpenPermalink = onOpenPermalink,
    onOpenConversation = onOpenConversation,
    canReact = rights.canReact,
    older = older,
    unseenReactions = unseenReactions,
    onReactionsSeen = onReactionsSeen,
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

/** What can be done with one picture of an album (#241): a long press on its tile. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun ImageActions(
  viewModel: MainViewModel,
  snapshot: ChatSnapshot?,
  image: FileInfo,
  maybeOnReply: ((FileInfo) -> Unit)?,
  onSave: () -> Unit,
  onOpen: () -> Unit,
  onDismiss: () -> Unit,
) {
  var isForwarding by remember { mutableStateOf(false) }
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp)) {
      maybeOnReply?.let { reply ->
        SheetItem(R.string.image_reply) {
          reply(image)
          onDismiss()
        }
      }
      SheetItem(R.string.image_forward_one) { isForwarding = true }
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        SheetItem(R.string.image_save) {
          onSave()
          onDismiss()
        }
      }
      SheetItem(R.string.file_open) {
        onOpen()
        onDismiss()
      }
    }
  }
  if (isForwarding && snapshot != null) {
    ForwardDialog(snapshot, onPick = { target ->
      viewModel.forwardFile(image, target)
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
  /** [maybeImage]: the one picture of an album the reply answers (#241). */
  data class Target(
    val conversation: String,
    val messageId: String,
    val excerpt: String,
    val authorName: String,
    val maybeImage: FileInfo? = null,
  )

  val current = kotlinx.coroutines.flow.MutableStateFlow<Target?>(null)

  fun set(conversation: String, message: Message, authorName: String) {
    current.value = Target(conversation, message.id, message.body, authorName)
  }

  fun setImage(file: FileInfo, authorName: String) {
    current.value = Target(file.conversation, file.message, file.fileName, authorName, file)
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
  val draftSave = remember(conversationId, maybeThread) { DraftSave() }
  // Kept in the view model: picked files survive looking at another conversation (until sent).
  var files by viewModel.pickedFiles.entry("$conversationId/${maybeThread.orEmpty()}", emptyList())
  var priority by rememberSaveable(conversationId) { mutableStateOf("normal") }
  var wantsAcknowledgement by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isSilent by rememberSaveable(conversationId) { mutableStateOf(false) }
  var alsoToChannel by rememberSaveable(conversationId) { mutableStateOf(false) }
  var isAttachOpen by remember { mutableStateOf(false) }
  // The picked picture being edited before it is sent.
  var maybeEditing by remember { mutableStateOf<PickedFile?>(null) }
  var isSendOptionsOpen by remember { mutableStateOf(false) }
  val replyTarget by ReplyTarget.current.collectAsState()
  val maybeReply = replyTarget?.takeIf { it.conversation == conversationId && maybeThread == null }
  // The camera, gallery and review flow (#1358); saved so it comes back after the app was killed.
  var maybeMediaFlow by rememberSaveable(conversationId, maybeThread, stateSaver = MediaFlowLaunch.saver) { mutableStateOf<MediaFlowLaunch?>(null) }
  val pickers = rememberAttachmentPickers(viewModel, onMediaFlow = { maybeMediaFlow = it }) { picked -> files = files + picked }
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
    val stored = viewModel.readChat { query(ChatQueries.draft(conversationId, maybeThread)).maybeBody }.orEmpty()
    if (text.isEmpty()) text = stored
    draftSave.stored(stored)
    isDraftLoaded = true
    // Opened from "Drafts & sent" to go on writing (#1220).
    if (OpenedDraft.current.value == OpenedDraft.Target(conversationId, maybeThread)) {
      OpenedDraft.current.value = null
      focus.requestFocus()
    }
  }
  val maybePendingShare by PendingShare.current.collectAsState()
  LaunchedEffect(maybePendingShare, isDraftLoaded) {
    // Text and files shared from another app land in the composer; the user sends them.
    val share = maybePendingShare?.takeIf { it.conversation == conversationId && maybeThread == null } ?: return@LaunchedEffect
    if (!isDraftLoaded) return@LaunchedEffect
    val (taken, failed) = share.content.files
      .map { it to viewModel.importSharedFile(share.content, it) }
      .partition { (_, maybePicked) -> maybePicked != null }
    val imported = taken.mapNotNull { (_, maybePicked) -> maybePicked }
    text = listOf(text, share.content.text).filter { it.isNotBlank() }.joinToString("\n")
    files = files + imported
    // At once: when files failed the picker comes back right now, and the draft's pause never ends.
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, text))
    draftSave.stored(text)
    val failedCount = failed.size
    if (failedCount > 0) {
      val failed = context.resources.getQuantityString(R.plurals.share_import_failed, failedCount, failedCount)
      Toast.makeText(context, failed, Toast.LENGTH_LONG).show()
    }
    // What failed stays in the inbox, and the picker comes back with it.
    viewModel.finishShare(share.content, failed.map { (file, _) -> file })
    // Cleared last: clearing restarts this effect, which would cancel the import.
    PendingShare.current.value = null
    focus.requestFocus()
  }
  LaunchedEffect(text) {
    if (!isDraftLoaded) return@LaunchedEffect
    val edit = draftSave.edit(text)
    delay(DRAFT_SAVE_DELAY_MS)
    draftSave.takeDue(edit)?.let { viewModel.act(ChatActions.setDraft(conversationId, maybeThread, it)) }
  }
  // Leaving the app saves at once: the process may be killed before the pause is over (#158).
  LifecycleEventEffect(Lifecycle.Event.ON_STOP) {
    draftSave.takePending()?.let { viewModel.act(ChatActions.setDraft(conversationId, maybeThread, it)) }
  }
  // So does leaving the conversation or closing the thread: the pause's wait goes with it (#1220).
  DisposableEffect(draftSave) {
    val conversation = conversationId
    val thread = maybeThread
    onDispose { draftSave.takePending()?.let { viewModel.act(ChatActions.setDraft(conversation, thread, it)) } }
  }
  val maybeQuery = Mentions.activeQuery(text)
  val members = snapshot?.conversation(conversationId)?.memberIds?.mapNotNull { snapshot.user(it) } ?: emptyList()
  fun sendOptions(sending: List<PickedFile>) = SendOptions(
    maybeThread = maybeThread,
    maybeReplyTo = maybeReply?.messageId,
    maybeReplyAttachment = maybeReply?.maybeImage?.fileId,
    priority = priority,
    wantsAcknowledgement = wantsAcknowledgement,
    isSilent = isSilent,
    alsoSendToChannel = alsoToChannel,
    files = sending,
  )

  fun resetOptions() {
    priority = "normal"
    wantsAcknowledgement = false
    isSilent = false
    ReplyTarget.current.value = null
    onSent()
  }

  /** A voice message goes alone, with the send options and the reply chosen (#204); the draft stays. */
  fun sendVoice(voice: PickedFile) {
    viewModel.act(ChatActions.send(conversationId, "", sendOptions(listOf(voice))))
    resetOptions()
  }

  fun send() {
    // `:)` and `:tada:` become emoji, as on desktop.
    viewModel.act(ChatActions.send(conversationId, emojiConvert(text.trim()), sendOptions(files)))
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, ""))
    draftSave.stored("")
    text = ""
    files = emptyList()
    priority = "normal"
    wantsAcknowledgement = false
    isSilent = false
    ReplyTarget.current.value = null
    onSent()
  }
  var isSendLaterOpen by remember { mutableStateOf(false) }
  // The time while @channel waits for its confirmation; none for a plain send.
  var maybeSendLaterAt by remember { mutableStateOf<Long?>(null) }
  // This text's message ID after a failed try, so a retry after a lost answer holds one message. Gone
  // once the server took it or the text changes: a later message with the same words is another one.
  var maybeScheduleAttempt by remember { mutableStateOf<Pair<String, String>?>(null) }
  fun schedule(dueAtMs: Long) {
    val id = maybeScheduleAttempt?.takeIf { it.first == text }?.second
      ?: java.util.UUID.randomUUID().toString().replace("-", "")
    maybeScheduleAttempt = null
    val options = SendOptions(
      maybeThread = maybeThread,
      maybeReplyTo = maybeReply?.messageId,
      maybeReplyAttachment = maybeReply?.maybeImage?.fileId,
      priority = priority,
      wantsAcknowledgement = wantsAcknowledgement,
      isSilent = isSilent,
      alsoSendToChannel = alsoToChannel,
      maybeClientMessageId = id,
    )
    val body = text
    // The server may refuse it: the text comes back, and the failure shows like any other.
    viewModel.act(ChatActions.schedule(conversationId, emojiConvert(body.trim()), options, dueAtMs)) {
      text = body
      maybeScheduleAttempt = body to id
    }
    viewModel.act(ChatActions.setDraft(conversationId, maybeThread, ""))
    draftSave.stored("")
    text = ""
    priority = "normal"
    wantsAcknowledgement = false
    isSilent = false
    ReplyTarget.current.value = null
    maybeSendLaterAt = null
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
          val maybeAt = maybeSendLaterAt
          if (maybeAt != null) schedule(maybeAt) else send()
        }) { Text(stringResource(R.string.send)) }
      },
      dismissButton = {
        TextButton(onClick = {
          isConfirmingChannel = false
          maybeSendLaterAt = null
        }) { Text(stringResource(R.string.cancel)) }
      },
    )
  }
  val canSend = text.isNotBlank() || files.isNotEmpty()
  val voice = rememberVoiceControls(viewModel, VoiceOwner(conversationId, maybeThread))
  val voicePolicy = snapshot?.security?.voiceMessagePolicy ?: VoiceMessagePolicy()
  // The record button stands in for the send button while there is nothing to send; a recording
  // or take of this composer keeps it whatever the policy says now.
  val showsVoice = voice.isActive || (voicePolicy.enabled && !canSend)
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
        reply.maybeImage?.let { image ->
          Box(Modifier.padding(end = 8.dp).size(28.dp).clip(RoundedCornerShape(4.dp))) {
            Thumbnail(image, ContentScale.Crop)
          }
        }
        Text(
          stringResource(R.string.replying_to, reply.authorName) + ": " + reply.excerpt,
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
      FlowRow(
        Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
      ) {
        for (file in files) {
          if (PictureEdits.isEditable(file.mediaType)) {
            // Pictures show as themselves: a tap opens the editor, the corner removes them.
            PictureAttachment(
              file = file,
              onEdit = { maybeEditing = file },
              onRemove = {
                files = files - file
                viewModel.discardFile(file)
              },
            )
            continue
          }
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
      // What the next send carries, as a chip that opens the options again.
      AssistChip(
        onClick = { isSendOptionsOpen = true },
        label = { Text(flags.joinToString(" · "), maxLines = 1, overflow = TextOverflow.Ellipsis) },
        leadingIcon = { Icon(painterResource(R.drawable.ic_notification), contentDescription = null, modifier = Modifier.size(16.dp)) },
        modifier = Modifier.padding(horizontal = 12.dp),
      )
    }
    ComposerField(
      text = text,
      onTextChange = {
        text = it
        if (maybeScheduleAttempt?.first != it) maybeScheduleAttempt = null
        val now = System.currentTimeMillis()
        if (isSharingTyping && shouldNotifyTyping(it, now, maybeLastTypingAt)) {
          maybeLastTypingAt = now
          viewModel.act(ChatActions.typing(conversationId))
        }
      },
      canSend = canSend,
      isSendWithEnter = isSendWithEnter,
      focus = focus,
      onAttach = { isAttachOpen = true },
      onCamera = pickers.openCamera,
      onSend = ::trySend,
      onSendOptions = { isSendOptionsOpen = true },
      maybeVoiceBar = if (voice.isActive) {
        { VoiceBar(viewModel, voice) }
      } else {
        null
      },
      maybeTrailing = if (showsVoice) {
        { modifier -> VoiceButton(viewModel, voice, voicePolicy, SEND_BUTTON_SIZE, ::sendVoice, modifier) }
      } else {
        null
      },
    )
  }
  if (isAttachOpen) AttachSheet(pickers, onDismiss = { isAttachOpen = false })
  maybeMediaFlow?.let { launch ->
    MediaFlow(
      viewModel = viewModel,
      key = MediaDraftKey(conversationId, maybeThread),
      launch = launch,
      conversationName = snapshot?.conversation(conversationId)?.name.orEmpty(),
      initialCaption = text,
      onClose = { maybeMediaFlow = null },
      onSend = { picked, caption ->
        maybeMediaFlow = null
        // The caption is the message's text: it goes with the pictures as one message.
        files = files + picked
        text = caption
        trySend()
      },
    )
  }
  maybeEditing?.let { editing ->
    PictureEditor(
      viewModel = viewModel,
      file = editing,
      onCancel = { maybeEditing = null },
      onDone = { edited ->
        files = files.map { if (it == editing) edited else it }
        viewModel.discardFile(editing)
        maybeEditing = null
      },
    )
  }
  if (isSendOptionsOpen) {
    SendOptionsSheet(onDismiss = { isSendOptionsOpen = false }) {
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
      SendLaterRow(
        viewModel = viewModel,
        canSchedule = text.isNotBlank() && files.isEmpty(),
        hasFiles = files.isNotEmpty(),
        onOpen = {
          isSendOptionsOpen = false
          isSendLaterOpen = true
        },
      )
    }
  }
  if (isSendLaterOpen) {
    SendLaterSheet(
      viewModel = viewModel,
      onPick = { dueAtMs ->
        isSendLaterOpen = false
        val confirmOver = viewModel.confirmChannelOver().toUInt()
        if (needsChannelConfirmation(text, members.size.toUInt(), confirmOver)) {
          maybeSendLaterAt = dueAtMs
          isConfirmingChannel = true
        } else {
          schedule(dueAtMs)
        }
      },
      onDismiss = { isSendLaterOpen = false },
    )
  }
}

/**
 * The message field as in the common messengers (#315, #704): a rounded pill with attach on the
 * left and the camera on the right inside it, and a round accent send button beside it. While there
 * is nothing to send, the button records a voice message (#204, [maybeTrailing]); a recording or
 * take shows in the pill instead of the field ([maybeVoiceBar]). A long-press on send opens how the
 * message is sent (priority, receipts, silent), as Telegram's and Signal's send buttons do.
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
  onSendOptions: () -> Unit,
  maybeVoiceBar: (@Composable () -> Unit)? = null,
  maybeTrailing: (@Composable (Modifier) -> Unit)? = null,
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
      if (maybeVoiceBar != null) {
        maybeVoiceBar()
        return@Surface
      }
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
    if (maybeTrailing != null) {
      maybeTrailing(Modifier.padding(start = 6.dp, bottom = 4.dp))
      return@Row
    }
    SendButton(canSend, onSend, onSendOptions)
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

/** A picked picture in the composer: its thumbnail opens the editor, the corner button removes it. */
@Composable
private fun PictureAttachment(file: PickedFile, onEdit: () -> Unit, onRemove: () -> Unit) {
  val maybePicture by produceState<ImageBitmap?>(null, file.path) { value = Pictures.file(file.path, ATTACHMENT_THUMB_PX) }
  val editLabel = stringResource(R.string.composer_edit_picture, file.fileName)
  Box(Modifier.size(ATTACHMENT_THUMB)) {
    Box(
      Modifier
        .matchParentSize()
        .clip(RoundedCornerShape(12.dp))
        .background(MaterialTheme.colorScheme.surfaceContainerHighest)
        .clickable(onClickLabel = editLabel, onClick = onEdit),
    ) {
      maybePicture?.let { Image(it, contentDescription = file.fileName, contentScale = ContentScale.Crop, modifier = Modifier.matchParentSize()) }
      Icon(
        Icons.Filled.Edit,
        contentDescription = null,
        tint = Color.White,
        modifier = Modifier
          .align(Alignment.BottomStart)
          .padding(4.dp)
          .background(Color.Black.copy(alpha = 0.5f), CircleShape)
          .padding(3.dp)
          .size(14.dp),
      )
    }
    Box(
      Modifier
        .align(Alignment.TopEnd)
        .offset(x = 6.dp, y = (-6).dp)
        .size(24.dp)
        .clip(CircleShape)
        .background(MaterialTheme.colorScheme.inverseSurface)
        .clickable(onClickLabel = stringResource(R.string.composer_remove_file, file.fileName), onClick = onRemove),
      contentAlignment = Alignment.Center,
    ) {
      Icon(
        Icons.Filled.Close,
        contentDescription = stringResource(R.string.composer_remove_file, file.fileName),
        tint = MaterialTheme.colorScheme.inverseOnSurface,
        modifier = Modifier.size(16.dp),
      )
    }
  }
}

private val ATTACHMENT_THUMB = 64.dp
private const val ATTACHMENT_THUMB_PX = 256

/** The round send button: tap sends, long-press (also with nothing typed yet) sets how. */
@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SendButton(canSend: Boolean, onSend: () -> Unit, onSendOptions: () -> Unit) {
  val haptics = LocalHapticFeedback.current
  val scheme = MaterialTheme.colorScheme
  Box(
    Modifier
      .padding(start = 6.dp, bottom = 4.dp)
      .size(SEND_BUTTON_SIZE)
      .clip(CircleShape)
      .background(if (canSend) scheme.primary else scheme.surfaceContainerHigh)
      .combinedClickable(
        role = Role.Button,
        onClickLabel = stringResource(R.string.send),
        onLongClickLabel = stringResource(R.string.send_options),
        onClick = { if (canSend) onSend() },
        onLongClick = {
          haptics.performHapticFeedback(HapticFeedbackType.LongPress)
          onSendOptions()
        },
      )
      .semantics { if (!canSend) disabled() },
    contentAlignment = Alignment.Center,
  ) {
    Icon(
      Icons.AutoMirrored.Filled.Send,
      contentDescription = stringResource(R.string.send),
      tint = if (canSend) scheme.onPrimary else scheme.onSurfaceVariant.copy(alpha = 0.6f),
    )
  }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SendOptionsSheet(onDismiss: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
  ModalBottomSheet(onDismissRequest = onDismiss) {
    Column(Modifier.padding(bottom = 24.dp), content = content)
  }
}

/** Priority, read confirmation, silent and "also send to channel": the send button's long-press. */
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

/**
 * Before a call: join with the camera on or muted. `isNotetakerInCall`: the running call has the
 * notetaker in it, which the dialog says before joining (e2ee.md §8.2).
 */
@Composable
internal fun PreJoinDialog(
  name: String,
  isNotetakerInCall: Boolean,
  runningCount: Int,
  onDismiss: () -> Unit,
  onJoin: (CallOptions) -> Unit,
) {
  var isCameraOn by rememberSaveable { mutableStateOf(false) }
  var isMuted by rememberSaveable { mutableStateOf(false) }
  AlertDialog(
    onDismissRequest = onDismiss,
    // A call that runs already is joined, not started: the dialog says so, and who is in it.
    title = { Text(stringResource(if (runningCount > 0) R.string.call_prejoin_join_title else R.string.call_prejoin_title, name)) },
    text = {
      Column {
        if (runningCount > 0) {
          Text(
            pluralStringResource(R.plurals.call_running_people, runningCount, runningCount),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(bottom = 8.dp),
          )
        }
        if (isNotetakerInCall) {
          Text(
            stringResource(R.string.call_prejoin_notetaker),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(bottom = 8.dp),
          )
        }
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

/**
 * A call runs in this conversation: who is in it and a Join button, or "Back to the call" when
 * this device is in it. Joining from here starts with the microphone on and the camera off.
 */
@Composable
private fun RunningCallBanner(viewModel: MainViewModel, conversationId: String, name: String, count: Int) {
  val calls = TeamCraftApplication.graph(LocalContext.current).calls
  val maybeState by calls.callState.collectAsState()
  val isHere = maybeState?.conversationId == conversationId && maybeState?.phase != CallPhase.ENDED
  if (count == 0 && !isHere) return
  Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
    Row(Modifier.padding(start = 16.dp, end = 8.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
      Icon(Icons.Filled.Call, contentDescription = null, tint = MaterialTheme.colorScheme.onSecondaryContainer, modifier = Modifier.size(18.dp))
      Text(
        if (isHere) stringResource(R.string.call_in_this_call) else pluralStringResource(R.plurals.call_running_people, count, count),
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSecondaryContainer,
        modifier = Modifier.weight(1f).padding(start = 8.dp),
      )
      if (isHere) {
        TextButton(onClick = { viewModel.showCall() }) { Text(stringResource(R.string.call_back_to)) }
      } else {
        Button(onClick = { viewModel.startCall(conversationId, name) }) { Text(stringResource(R.string.call_prejoin_join)) }
      }
    }
  }
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
