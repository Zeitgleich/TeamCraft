package de.zeitgleich.teamcraft.ui.screens

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.BitmapFactory
import android.net.Uri
import android.text.format.DateUtils
import android.text.format.Formatter
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SmallFloatingActionButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import java.io.File
import java.time.LocalDate
import java.time.ZoneId
import kotlin.math.roundToInt
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.LinkPreview
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.ui.FeedItem
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.feedItems
import de.zeitgleich.teamcraft.ui.TextSpan
import de.zeitgleich.teamcraft.ui.textSpans
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.isOpenableLink

/** The reaction a double tap toggles, as in most chat apps. */
private const val QUICK_REACTION = "👍"

/** How long a message opened from a link stays highlighted. */
private const val HIGHLIGHT_MS = 2500L

/** How far a message is dragged to the right before letting go replies to it. */
private val SWIPE_REPLY_THRESHOLD = 72.dp

/**
 * A conversation's messages with date chips, the unread line and grouped follow-ups. Opens at the
 * first unread message (or the end), follows new messages only while the user is at the bottom,
 * and otherwise offers a jump-down button with the number of messages below.
 */
@Composable
fun MessageFeed(
  viewModel: MainViewModel,
  snapshot: ChatSnapshot?,
  messages: List<Message>,
  extras: Map<String, MessageExtras>,
  maybeFirstUnread: String?,
  onOpenThread: ((String) -> Unit)?,
  maybeOnReply: ((Message) -> Unit)?,
  onLongPress: (Message) -> Unit,
  modifier: Modifier = Modifier,
  maybeFocusMessage: String? = null,
  onOpenPermalink: (String) -> Unit = {},
) {
  // Newest first in a reversed list: the bottom stays anchored when the keyboard opens.
  val items = remember(messages, maybeFirstUnread) { feedItems(messages, maybeFirstUnread).asReversed() }
  val listState = rememberLazyListState()
  val scope = rememberCoroutineScope()
  var hasScrolledInitially by remember { mutableStateOf(false) }
  var maybeLastMessageId by remember { mutableStateOf<String?>(null) }
  var maybeViewed by remember { mutableStateOf<ViewedImage?>(null) }
  var maybeWaitingFor by remember { mutableStateOf<String?>(null) }
  var maybeHighlighted by remember(maybeFocusMessage) { mutableStateOf(maybeFocusMessage) }
  LaunchedEffect(maybeHighlighted) {
    if (maybeHighlighted != null) {
      delay(HIGHLIGHT_MS)
      maybeHighlighted = null
    }
  }
  val isAtBottom by remember { derivedStateOf { listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset < 8 } }
  val belowCount by remember(items) { derivedStateOf { items.messagesBefore(listState.firstVisibleItemIndex) } }
  LaunchedEffect(items) {
    if (items.isEmpty()) return@LaunchedEffect
    val newest = messages.last()
    if (!hasScrolledInitially) {
      hasScrolledInitially = true
      // A message link opens at its message; otherwise at the first unread one.
      val focusIndex = maybeFocusMessage?.let { id -> items.indexOfFirst { it is FeedItem.Entry && it.message.id == id } } ?: -1
      val unreadIndex = items.indexOf(FeedItem.Unread)
      when {
        focusIndex >= 0 -> showAtTop(listState, focusIndex)
        unreadIndex > 0 -> showAtTop(listState, unreadIndex)
      }
    } else if (newest.id != maybeLastMessageId && (isAtBottom || newest.author == snapshot?.me?.id)) {
      // Own messages always come into view; others' only while reading at the bottom.
      listState.animateScrollToItem(0)
    }
    maybeLastMessageId = newest.id
  }
  // A tapped image that was still being downloaded opens once it is here.
  LaunchedEffect(extras, maybeWaitingFor) {
    val waiting = maybeWaitingFor ?: return@LaunchedEffect
    val file = extras.values.flatMap { it.files }.firstOrNull { it.fileId == waiting } ?: return@LaunchedEffect
    val path = file.transfer.path
    if (file.transfer.state == "local" && path != null) {
      maybeViewed = ViewedImage(path, file.fileName, file.mediaType)
      maybeWaitingFor = null
    } else if (file.transfer.state == "failed") {
      maybeWaitingFor = null
    }
  }
  val context = LocalContext.current
  val onOpenFile: (FileInfo) -> Unit = { file ->
    openOrDownload(viewModel, file, context) { maybeImage ->
      if (maybeImage != null) maybeViewed = maybeImage else maybeWaitingFor = file.fileId.takeIf { file.isImage }
    }
  }
  Box(modifier.fillMaxSize()) {
    LazyColumn(Modifier.fillMaxSize(), state = listState, reverseLayout = true) {
      items(items, key = { it.key }) { item ->
        when (item) {
          is FeedItem.Day -> DayChip(item.date)
          FeedItem.Unread -> UnreadDivider()
          is FeedItem.Entry -> SwipeToReply(item.message, maybeOnReply) {
            MessageRow(
              viewModel = viewModel,
              message = item.message,
              maybeExtras = extras[item.message.id],
              snapshot = snapshot,
              isContinuation = item.isContinuation,
              onOpenThread = onOpenThread,
              onOpenFile = onOpenFile,
              onLongPress = { onLongPress(item.message) },
              maybeOnReply = maybeOnReply,
              isHighlighted = item.message.id == maybeHighlighted,
              onOpenPermalink = onOpenPermalink,
            )
          }
        }
      }
    }
    AnimatedVisibility(
      visible = !isAtBottom && items.isNotEmpty(),
      enter = fadeIn(),
      exit = fadeOut(),
      modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
    ) {
      JumpDownButton(belowCount) { scope.launch { listState.animateScrollToItem(0) } }
    }
  }
  maybeViewed?.let { image -> ImageViewer(image) { maybeViewed = null } }
}

/** Messages newer than the item at `index` of the newest-first list (the "below" count). */
private fun List<FeedItem>.messagesBefore(index: Int): Int = take(index).count { it is FeedItem.Entry }

/** Puts the item at `index` (newest-first list) at the top of the screen, newer messages below it. */
private suspend fun showAtTop(listState: LazyListState, index: Int) {
  listState.scrollToItem(index)
  val info = listState.layoutInfo
  val itemSize = info.visibleItemsInfo.firstOrNull { it.index == index }?.size ?: 0
  // In a reversed list a negative scroll moves toward the newest messages.
  listState.scrollBy(-(info.viewportEndOffset - info.viewportStartOffset - itemSize).toFloat())
}

/** Images open in the viewer, other local files in another app; anything else is downloaded first. */
private fun openOrDownload(viewModel: MainViewModel, file: FileInfo, context: Context, onImage: (ViewedImage?) -> Unit) {
  val path = file.transfer.path
  when {
    file.transfer.state == "local" && path != null && file.isImage -> onImage(ViewedImage(path, file.fileName, file.mediaType))
    file.transfer.state == "local" && path != null -> openFile(context, File(path), file.mediaType)
    file.transfer.state == "downloading" -> onImage(null)
    else -> {
      viewModel.act(ChatActions.download(file.conversation, file.message))
      onImage(null)
    }
  }
}

@Composable
private fun JumpDownButton(count: Int, onClick: () -> Unit) {
  val label = if (count > 0) pluralStringResource(R.plurals.jump_down_new, count, count) else stringResource(R.string.jump_down)
  BadgedBox(badge = {
    if (count > 0) {
      Badge { Text(if (count > 99) "99+" else count.toString()) }
    }
  }) {
    SmallFloatingActionButton(onClick = onClick, modifier = Modifier.semantics { contentDescription = label }) {
      Icon(Icons.Filled.KeyboardArrowDown, contentDescription = null)
    }
  }
}

@Composable
private fun DayChip(date: LocalDate) {
  val context = LocalContext.current
  val today = LocalDate.now()
  val text = when (date) {
    today -> stringResource(R.string.day_today)
    today.minusDays(1) -> stringResource(R.string.day_yesterday)
    else -> {
      val millis = date.atStartOfDay(ZoneId.systemDefault()).toInstant().toEpochMilli()
      val flags = DateUtils.FORMAT_SHOW_DATE or DateUtils.FORMAT_SHOW_WEEKDAY or
        if (date.year == today.year) DateUtils.FORMAT_NO_YEAR else DateUtils.FORMAT_SHOW_YEAR
      DateUtils.formatDateTime(context, millis, flags)
    }
  }
  Box(Modifier.fillMaxWidth().padding(vertical = 8.dp), contentAlignment = Alignment.Center) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(12.dp)) {
      Text(
        text,
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(horizontal = 12.dp, vertical = 4.dp),
      )
    }
  }
}

@Composable
private fun UnreadDivider() {
  Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
    HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.error)
    Text(
      stringResource(R.string.new_messages),
      color = MaterialTheme.colorScheme.error,
      style = MaterialTheme.typography.labelSmall,
      modifier = Modifier.padding(horizontal = 8.dp),
    )
    HorizontalDivider(Modifier.weight(1f), color = MaterialTheme.colorScheme.error)
  }
}

/** Drag a message to the right to reply to it (where replies are possible). */
@Composable
private fun SwipeToReply(message: Message, maybeOnReply: ((Message) -> Unit)?, content: @Composable () -> Unit) {
  if (maybeOnReply == null) {
    content()
    return
  }
  val haptics = LocalHapticFeedback.current
  val thresholdPx = with(LocalDensity.current) { SWIPE_REPLY_THRESHOLD.toPx() }
  val offset = remember { Animatable(0f) }
  val scope = rememberCoroutineScope()
  var hasPassedThreshold by remember { mutableStateOf(false) }
  Box(
    Modifier.pointerInput(message.id) {
      detectHorizontalDragGestures(
        onDragEnd = {
          if (hasPassedThreshold) maybeOnReply(message)
          hasPassedThreshold = false
          scope.launch { offset.animateTo(0f) }
        },
        onDragCancel = {
          hasPassedThreshold = false
          scope.launch { offset.animateTo(0f) }
        },
      ) { change, dragAmount ->
        val next = (offset.value + dragAmount).coerceIn(0f, thresholdPx * 1.5f)
        if (next != offset.value) change.consume()
        scope.launch { offset.snapTo(next) }
        val isPast = next >= thresholdPx
        if (isPast && !hasPassedThreshold) haptics.performHapticFeedback(HapticFeedbackType.LongPress)
        hasPassedThreshold = isPast
      }
    },
  ) {
    Text(
      "↩",
      style = MaterialTheme.typography.titleLarge,
      color = MaterialTheme.colorScheme.primary,
      modifier = Modifier
        .align(Alignment.CenterStart)
        .padding(start = 16.dp)
        .graphicsLayer { alpha = (offset.value / thresholdPx).coerceIn(0f, 1f) },
    )
    Box(Modifier.offset { IntOffset(offset.value.roundToInt(), 0) }) { content() }
  }
}

@OptIn(ExperimentalFoundationApi::class, ExperimentalLayoutApi::class)
@Composable
private fun MessageRow(
  viewModel: MainViewModel,
  message: Message,
  maybeExtras: MessageExtras?,
  snapshot: ChatSnapshot?,
  isContinuation: Boolean,
  onOpenThread: ((String) -> Unit)?,
  onOpenFile: (FileInfo) -> Unit,
  onLongPress: () -> Unit,
  maybeOnReply: ((Message) -> Unit)?,
  isHighlighted: Boolean,
  onOpenPermalink: (String) -> Unit,
) {
  if (message.maybeSystemEvent != null || maybeExtras?.maybeWorkspaceEvent != null) {
    // Joins, renames and the like: a quiet centered line, not a post.
    Text(
      systemText(message, maybeExtras, snapshot),
      style = MaterialTheme.typography.bodySmall,
      fontStyle = FontStyle.Italic,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
      modifier = Modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 6.dp),
    )
    return
  }
  val context = LocalContext.current
  val haptics = LocalHapticFeedback.current
  val isMine = message.author == snapshot?.me?.id
  val replyLabel = stringResource(R.string.action_reply)
  val reactLabel = stringResource(R.string.react_quick, QUICK_REACTION)
  val moreLabel = stringResource(R.string.more)
  Column(
    Modifier
      .fillMaxWidth()
      .background(
        animateColorAsState(
          if (isHighlighted) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.background,
          label = "highlight",
        ).value,
      )
      .combinedClickable(
        onClick = {},
        onDoubleClick = { viewModel.act(ChatActions.react(message.conversation, message.id, QUICK_REACTION)) },
        onLongClick = {
          haptics.performHapticFeedback(HapticFeedbackType.LongPress)
          onLongPress()
        },
      )
      .semantics {
        customActions = listOfNotNull(
          maybeOnReply?.let { reply -> CustomAccessibilityAction(replyLabel) { reply(message); true } },
          CustomAccessibilityAction(reactLabel) {
            viewModel.act(ChatActions.react(message.conversation, message.id, QUICK_REACTION))
            true
          },
          CustomAccessibilityAction(moreLabel) { onLongPress(); true },
        )
      }
      .padding(start = 16.dp, end = 16.dp, top = if (isContinuation) 2.dp else 10.dp, bottom = 2.dp),
  ) {
    maybeExtras?.maybeForwarded?.let { forwarded ->
      Text(
        stringResource(R.string.forwarded_from, forwarded.authorName),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
    // A rejected message says "not sent" below instead of "sending…" forever.
    val isSending = message.isPending && maybeExtras?.isSendFailed != true
    if (!isContinuation) {
      Row(verticalAlignment = Alignment.Bottom) {
        Text(
          if (isMine) stringResource(R.string.you) else snapshot?.name(message.author).orEmpty(),
          fontWeight = FontWeight.Bold,
          modifier = Modifier.padding(end = 8.dp),
        )
        Text(
          if (isSending) stringResource(R.string.pending) else DateUtils.formatDateTime(context, message.sentAt, DateUtils.FORMAT_SHOW_TIME),
          style = MaterialTheme.typography.bodySmall,
          color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Markers(maybeExtras)
      }
    } else if (isSending || maybeExtras?.isPinned == true || (maybeExtras != null && maybeExtras.priority != "normal")) {
      Row(verticalAlignment = Alignment.Bottom) {
        if (isSending) {
          Text(stringResource(R.string.pending), style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        Markers(maybeExtras)
      }
    }
    maybeExtras?.maybeReply?.let { reply -> ReplyQuote(snapshot?.name(reply.author).orEmpty(), reply.excerpt) }
    when {
      maybeExtras?.isDeleted == true -> Text(stringResource(R.string.message_deleted), fontStyle = FontStyle.Italic)
      message.body.isNotEmpty() && !isOnlyFileLabel(message.body, maybeExtras) ->
        MessageText(message, snapshot, onOpenPermalink)
    }
    if (maybeExtras?.isSendFailed == true) NotSentLine(viewModel, message)
    maybeExtras?.files?.forEach { file -> FileAttachment(file, onOpenFile) }
    maybeExtras?.maybeLinkPreview?.let { LinkPreviewCard(it) }
    if (message.reactions.isNotEmpty()) {
      FlowRow(horizontalArrangement = Arrangement.spacedBy(4.dp)) {
        for (reaction in message.reactions) {
          FilterChip(
            selected = reaction.reactedByMe,
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
        fontWeight = FontWeight.Bold,
        modifier = Modifier.clickable { onOpenThread(message.id) }.padding(vertical = 4.dp),
      )
    }
  }
}

/** The core labels file messages "📎 name"; with the file shown below, the label is noise. */
private fun isOnlyFileLabel(body: String, maybeExtras: MessageExtras?): Boolean =
  maybeExtras?.files?.any { body.trim() == "📎 ${it.fileName}" } == true

/** An own message the server rejected: say so, and offer to send it again or drop it. */
@Composable
private fun NotSentLine(viewModel: MainViewModel, message: Message) {
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      "⚠ " + stringResource(R.string.not_sent),
      color = MaterialTheme.colorScheme.error,
      style = MaterialTheme.typography.labelMedium,
      modifier = Modifier.weight(1f, fill = false),
    )
    TextButton(onClick = { viewModel.act(ChatActions.retrySend(message.conversation, message.id)) }) {
      Text(stringResource(R.string.not_sent_retry))
    }
    TextButton(onClick = { viewModel.act(ChatActions.discardUnsent(message.conversation, message.id)) }) {
      Text(stringResource(R.string.not_sent_delete), color = MaterialTheme.colorScheme.error)
    }
  }
}

@Composable
private fun Markers(maybeExtras: MessageExtras?) {
  if (maybeExtras?.isPinned == true) Text(" 📌", style = MaterialTheme.typography.bodySmall)
  if (maybeExtras?.priority == "urgent") {
    Text(" " + stringResource(R.string.priority_urgent), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
  } else if (maybeExtras?.priority == "important") {
    Text(" " + stringResource(R.string.priority_important), color = MaterialTheme.colorScheme.tertiary, style = MaterialTheme.typography.labelSmall)
  }
}

@Composable
private fun ReplyQuote(author: String, excerpt: String) {
  Row(Modifier.padding(vertical = 2.dp)) {
    Box(Modifier.width(3.dp).heightIn(min = 32.dp).background(MaterialTheme.colorScheme.primary))
    Column(Modifier.padding(start = 8.dp)) {
      Text(author, style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.primary)
      Text(
        excerpt,
        maxLines = 2,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
    }
  }
}

/**
 * The body with tappable web links (only those the shared rule allows to open), message links that
 * open in the app, and highlighted mentions (stronger for the user's own name).
 */
@Composable
private fun MessageText(message: Message, snapshot: ChatSnapshot?, onOpenPermalink: (String) -> Unit) {
  val context = LocalContext.current
  val style = SpanColors(
    link = MaterialTheme.colorScheme.primary,
    mention = MaterialTheme.colorScheme.primary,
    mentionMe = LocalTeamCraftColors.current.mentionText,
    mentionMeBackground = LocalTeamCraftColors.current.mentionBackground,
  )
  val mutedColor = MaterialTheme.colorScheme.onSurfaceVariant
  val edited = if (message.isEdited) " " + stringResource(R.string.edited) else ""
  val usernames = remember(snapshot) { snapshot?.users.orEmpty().map { it.username.lowercase() }.toSet() }
  val myUsername = snapshot?.me?.username.orEmpty()
  val text = remember(message.body, edited, style, mutedColor, usernames, myUsername) {
    val spans = textSpans(message.body, usernames, myUsername)
    styled(message.body, spans, style, onOpenWeb = { url -> openLink(context, url) }, onOpenPermalink = onOpenPermalink).let { body ->
      if (edited.isEmpty()) body else buildAnnotatedString {
        append(body)
        pushStyle(SpanStyle(color = mutedColor, fontStyle = FontStyle.Italic))
        append(edited)
        pop()
      }
    }
  }
  Text(text)
}

private data class SpanColors(val link: Color, val mention: Color, val mentionMe: Color, val mentionMeBackground: Color)

private fun styled(
  body: String,
  spans: List<TextSpan>,
  colors: SpanColors,
  onOpenWeb: (String) -> Unit,
  onOpenPermalink: (String) -> Unit,
): AnnotatedString =
  buildAnnotatedString {
    append(body)
    val linkStyle = TextLinkStyles(SpanStyle(color = colors.link, textDecoration = TextDecoration.Underline))
    for (span in spans) {
      val end = span.range.last + 1
      when (span) {
        is TextSpan.WebLink -> if (isOpenableLink(span.url)) {
          addLink(LinkAnnotation.Clickable(tag = span.url, styles = linkStyle) { onOpenWeb(span.url) }, span.range.first, end)
        }
        is TextSpan.Permalink ->
          addLink(LinkAnnotation.Clickable(tag = span.link, styles = linkStyle) { onOpenPermalink(span.link) }, span.range.first, end)
        is TextSpan.Mention -> addStyle(
          if (span.isMe) {
            SpanStyle(color = colors.mentionMe, background = colors.mentionMeBackground, fontWeight = FontWeight.Bold)
          } else {
            SpanStyle(color = colors.mention, fontWeight = FontWeight.Bold)
          },
          span.range.first,
          end,
        )
      }
    }
  }

/** Opens a web link in the browser; only links the shared rule allows. */
fun openLink(context: Context, url: String) {
  if (!isOpenableLink(url)) return
  try {
    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
  } catch (failure: ActivityNotFoundException) {
    // No app handles the link; nothing sensible to open, so leave it.
  } catch (failure: SecurityException) {
    // The handling app refused to be started from here; leave it.
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

/** Images as a large preview (tap for full screen), other files as a row with name and size. */
@Composable
private fun FileAttachment(file: FileInfo, onOpen: (FileInfo) -> Unit) {
  val context = LocalContext.current
  val maybeThumbnail = rememberThumbnail(file.maybeThumbnailPng)
  Column(
    Modifier
      .padding(top = 4.dp)
      .widthIn(max = 280.dp)
      .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
      .clickable(onClickLabel = stringResource(R.string.file_open)) { onOpen(file) }
      .padding(if (maybeThumbnail != null) 4.dp else 8.dp),
  ) {
    if (maybeThumbnail != null) {
      Image(
        maybeThumbnail,
        contentDescription = file.fileName,
        contentScale = ContentScale.Crop,
        modifier = Modifier.fillMaxWidth().heightIn(max = 220.dp),
      )
    }
    if (maybeThumbnail == null || !file.isImage) {
      Row(verticalAlignment = Alignment.CenterVertically) {
        Text("📎", modifier = Modifier.padding(end = 6.dp))
        Column {
          Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium)
          Text(
            Formatter.formatShortFileSize(context, file.sizeBytes),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
          )
        }
      }
    }
    file.transfer.maybeProgress?.let { progress ->
      LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth().padding(top = 4.dp))
    }
    if (file.transfer.state == "failed") {
      Text(stringResource(R.string.file_failed), color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.labelSmall)
    }
  }
}

/** Decodes a PNG thumbnail once per content, not on every recomposition. */
@Composable
fun rememberThumbnail(maybePng: List<Int>?): ImageBitmap? = remember(maybePng) {
  maybePng?.let { bytes ->
    val array = ByteArray(bytes.size) { bytes[it].toByte() }
    BitmapFactory.decodeByteArray(array, 0, array.size)?.asImageBitmap()
  }
}

@Composable
private fun LinkPreviewCard(preview: LinkPreview) {
  val context = LocalContext.current
  val maybeImage = rememberThumbnail(preview.maybeImage)
  Row(
    Modifier
      .padding(top = 4.dp)
      .widthIn(max = 320.dp)
      .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(8.dp))
      .clickable { openLink(context, preview.url) },
  ) {
    Box(Modifier.width(3.dp).heightIn(min = 48.dp).background(MaterialTheme.colorScheme.primary))
    Column(Modifier.padding(8.dp)) {
      maybeImage?.let { Image(it, contentDescription = null, modifier = Modifier.heightIn(max = 120.dp).size(width = 280.dp, height = 120.dp), contentScale = ContentScale.Crop) }
      preview.maybeSiteName?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant) }
      preview.maybeTitle?.let { Text(it, fontWeight = FontWeight.Bold, maxLines = 2, overflow = TextOverflow.Ellipsis) }
      preview.maybeDescription?.let { Text(it, style = MaterialTheme.typography.bodySmall, maxLines = 3, overflow = TextOverflow.Ellipsis) }
    }
  }
}
