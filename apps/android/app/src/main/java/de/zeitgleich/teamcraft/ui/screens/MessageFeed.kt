package de.zeitgleich.teamcraft.ui.screens

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.text.format.DateUtils
import android.text.format.Formatter
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.Animatable
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.gestures.scrollBy
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.ExperimentalLayoutApi
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.IntrinsicSize
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Done
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import java.io.File
import java.time.Instant
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
import de.zeitgleich.teamcraft.core.ReplyPreview
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.ui.FeedItem
import de.zeitgleich.teamcraft.ui.FeedScrollAction
import de.zeitgleich.teamcraft.ui.FeedScrollEvent
import de.zeitgleich.teamcraft.ui.FeedScrollState
import de.zeitgleich.teamcraft.ui.FirstUnread
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.MarkupColors
import de.zeitgleich.teamcraft.ui.MentionNames
import de.zeitgleich.teamcraft.ui.Pictures
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.bubbleCorners
import de.zeitgleich.teamcraft.ui.decideFeedScroll
import de.zeitgleich.teamcraft.ui.feedItems
import de.zeitgleich.teamcraft.ui.isNearBottom
import de.zeitgleich.teamcraft.ui.newerMessages
import de.zeitgleich.teamcraft.ui.rememberThumbnail
import de.zeitgleich.teamcraft.ui.showsSenders
import de.zeitgleich.teamcraft.ui.thumbnailKey
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.isOpenableLink

/** The reaction a double tap toggles, as in most chat apps. */
private const val QUICK_REACTION = "👍"

/** How long a message opened from a link stays highlighted. */
private const val HIGHLIGHT_MS = 2500L

/** Scrolled less than this from the end still counts as "at the end" for the jump-down button. */
private const val AT_BOTTOM_SLOP_PX = 8

/** The pill counts new messages up to this, then says "99+". */
private const val MAX_SHOWN_UNSEEN = 99

/** How long the floating day pill stays after scrolling stopped. */
private const val DAY_PILL_LINGER_MS = 1200L

/** A bubble takes at most this share of the feed's width, and never more than [BUBBLE_MAX_WIDTH]. */
private const val BUBBLE_WIDTH_SHARE = 0.8f
private val BUBBLE_MAX_WIDTH = 520.dp

/** The tallest an image preview gets; its width is the bubble's (#315, #704). */
private val PREVIEW_MAX_HEIGHT = 340.dp

/** From this size on, an upload's progress is told in bytes ("12 MB / 48 MB"), below in percent. */
private const val LARGE_TRANSFER_BYTES = 1024L * 1024L

/** Panoramas and tall screenshots are cut to these shapes (width over height) rather than shrunk to a line. */
private const val MIN_PREVIEW_RATIO = 0.6f
private const val MAX_PREVIEW_RATIO = 2.2f

/** The shape of a preview whose thumbnail can't be read. */
private const val DEFAULT_PREVIEW_RATIO = 4f / 3f

/** How far a message is dragged to the right before letting go replies to it. */
private val SWIPE_REPLY_THRESHOLD = 72.dp

private val AVATAR_SIZE = 32.dp
private val AVATAR_GAP = 6.dp
private val MEDIA_CORNER = 14.dp

/**
 * A conversation's messages as chat bubbles (issue #704): own messages on the end side in the
 * accent colour, others on the start side in a neutral bubble with name and avatar once per group
 * (channels and groups). Date pills, the unread line, a floating date while scrolling. It follows
 * the conversation like a messenger (#755, [decideFeedScroll]): opens at the first unread message
 * (or the end), jumps to the end on send, stays pinned there while the user reads at the end, and
 * otherwise shows an "N new" pill instead of moving.
 */
@Composable
fun MessageFeed(
  viewModel: MainViewModel,
  snapshot: ChatSnapshot?,
  messages: List<Message>,
  extras: Map<String, MessageExtras>,
  firstUnread: FirstUnread,
  sentCount: Int,
  onOpenThread: ((String) -> Unit)?,
  maybeOnReply: ((Message) -> Unit)?,
  onLongPress: (Message) -> Unit,
  modifier: Modifier = Modifier,
  maybeFocusMessage: String? = null,
  onOpenPermalink: (String) -> Unit = {},
) {
  // Newest first in a reversed list: the bottom stays anchored when the keyboard opens or a picture
  // above grows, and the rows keep their place by key (the message ID) when new ones arrive.
  val items = remember(messages, firstUnread) { feedItems(messages, firstUnread.maybeMessageId).asReversed() }
  val listState = rememberLazyListState()
  val scope = rememberCoroutineScope()
  // Saveable: coming back from a thread or a rotation doesn't jump to the unread line again.
  var scrollState by rememberSaveable(stateSaver = FeedScrollStateSaver) { mutableStateOf(FeedScrollState()) }
  val newestSeen = remember { NewestSeen() }
  fun decide(event: FeedScrollEvent, isNearBottom: Boolean): FeedScrollAction {
    val decision = decideFeedScroll(scrollState, event, isNearBottom)
    scrollState = decision.state
    return decision.action
  }
  // Measured while composing the new rows, before the list lays them out: afterwards the list has
  // kept its place on the previous newest row, and the end would already look far away.
  val wasNearBottom = remember(items) { Snapshot.withoutReadObservation { listState.isNearBottom() } }
  // Saveable: a rotation keeps the viewer open.
  var maybeViewedId by rememberSaveable { mutableStateOf<String?>(null) }
  var maybeHighlighted by remember(maybeFocusMessage) { mutableStateOf(maybeFocusMessage) }
  LaunchedEffect(maybeHighlighted) {
    if (maybeHighlighted != null) {
      delay(HIGHLIGHT_MS)
      maybeHighlighted = null
    }
  }
  val showsJumpDown by remember {
    derivedStateOf { listState.firstVisibleItemIndex > 0 || listState.firstVisibleItemScrollOffset > AT_BOTTOM_SLOP_PX }
  }
  LaunchedEffect(items, firstUnread) {
    // The opening waits for the unread position, or it would land at the end and stay there.
    if (items.isEmpty() || (!scrollState.isOpened && firstUnread == FirstUnread.Loading)) return@LaunchedEffect
    val maybePrevious = newestSeen.maybeMessage
    newestSeen.maybeMessage = messages.last()
    val action = if (!scrollState.isOpened) {
      val focusIndex = maybeFocusMessage?.let { id -> items.indexOfFirst { it is FeedItem.Entry && it.message.id == id } } ?: -1
      val unreadIndex = items.indexOf(FeedItem.Unread)
      decide(FeedScrollEvent.Opened(focusIndex.takeIf { it >= 0 }, unreadIndex.takeIf { it >= 0 }), wasNearBottom)
    } else {
      val newer = newerMessages(messages, maybePrevious)
      if (newer.isEmpty()) return@LaunchedEffect
      val me = snapshot?.me?.id
      val event = FeedScrollEvent.NewMessages(fromOthers = newer.count { it.author != me }, hasOwn = newer.any { it.author == me })
      decide(event, wasNearBottom)
    }
    listState.perform(action)
  }
  LaunchedEffect(sentCount) {
    if (sentCount == 0) return@LaunchedEffect
    listState.perform(decide(FeedScrollEvent.Sent, listState.isNearBottom()))
  }
  LaunchedEffect(listState) {
    snapshotFlow { listState.firstVisibleItemIndex == 0 }.collect { isNewestVisible ->
      if (isNewestVisible) decide(FeedScrollEvent.ReachedBottom, isNearBottom = true)
    }
  }
  val context = LocalContext.current
  val onOpenFile: (FileInfo) -> Unit = remember(viewModel, context) {
    { file ->
      // Images open in the viewer at once (it shows the thumbnail and fetches the full picture).
      if (file.isImage && file.maybeThumbnail != null) maybeViewedId = file.fileId else openOrDownload(viewModel, file, context)
    }
  }
  // Once per snapshot, not per row: mentions are matched against every user's name.
  val names = remember(snapshot) {
    MentionNames(snapshot?.users.orEmpty().map { it.username.lowercase() }.toSet(), snapshot?.me?.username.orEmpty())
  }
  val showsSenders = showsSenders(messages.firstOrNull()?.let { snapshot?.conversation(it.conversation)?.kind })
  BoxWithConstraints(modifier.fillMaxSize()) {
    val bubbleMax = minOf(maxWidth * BUBBLE_WIDTH_SHARE, BUBBLE_MAX_WIDTH)
    val look = FeedLook(bubbleMax = bubbleMax, showsSenders = showsSenders, names = names)
    LazyColumn(Modifier.fillMaxSize(), state = listState, reverseLayout = true) {
      // Rows of one kind reuse each other's layout when they scroll in.
      items(items, key = { it.key }, contentType = { it.contentType }) { item ->
        when (item) {
          is FeedItem.Day -> DayPill(item.date, Modifier.fillMaxWidth().padding(vertical = 8.dp))
          FeedItem.Unread -> UnreadDivider()
          is FeedItem.Entry -> SwipeToReply(item.message, maybeOnReply) {
            MessageRow(
              viewModel = viewModel,
              entry = item,
              maybeExtras = extras[item.message.id],
              extras = extras,
              snapshot = snapshot,
              look = look,
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
    FloatingDay(listState, items, Modifier.align(Alignment.TopCenter).padding(top = 8.dp))
    AnimatedVisibility(
      visible = (showsJumpDown || scrollState.unseenCount > 0) && items.isNotEmpty(),
      enter = fadeIn(),
      exit = fadeOut(),
      modifier = Modifier.align(Alignment.BottomEnd).padding(16.dp),
    ) {
      JumpDownButton(scrollState.unseenCount) {
        decide(FeedScrollEvent.ReachedBottom, isNearBottom = true)
        scope.launch { listState.animateScrollToItem(0) }
      }
    }
  }
  maybeViewedId?.let { fileId ->
    ImageViewer(
      // Oldest first, so swiping to the right goes back in time as in the feed above.
      images = messages.flatMap { message -> extras[message.id]?.files.orEmpty() }.filter { it.isImage && it.maybeThumbnail != null },
      startFileId = fileId,
      onDownload = { file -> viewModel.act(ChatActions.download(file.conversation, file.message)) },
      onDismiss = { maybeViewedId = null },
    )
  }
}

/** What every bubble of a feed shares. */
private class FeedLook(val bubbleMax: Dp, val showsSenders: Boolean, val names: MentionNames)

/** The newest message the feed has handled; a plain holder, reading it never recomposes. */
private class NewestSeen(var maybeMessage: Message? = null)

private val FeedScrollStateSaver = listSaver<FeedScrollState, Any>(
  save = { listOf(it.isOpened, it.isAwaitingOwnMessage, it.unseenCount) },
  restore = { FeedScrollState(isOpened = it[0] as Boolean, isAwaitingOwnMessage = it[1] as Boolean, unseenCount = it[2] as Int) },
)

/** [isNearBottom] for the list as last laid out. */
private fun LazyListState.isNearBottom(): Boolean {
  val info = layoutInfo
  val visible = info.visibleItemsInfo
  val averageRowPx = if (visible.isEmpty()) 0 else visible.sumOf { it.size } / visible.size
  return isNearBottom(firstVisibleItemIndex, firstVisibleItemScrollOffset, averageRowPx, info.viewportEndOffset - info.viewportStartOffset)
}

private suspend fun LazyListState.perform(action: FeedScrollAction) {
  when (action) {
    FeedScrollAction.None -> {}
    is FeedScrollAction.ShowAtTop -> showAtTop(this, action.index)
    is FeedScrollAction.ToBottom -> if (action.isAnimated) animateScrollToItem(0) else scrollToItem(0)
  }
}

/** Puts the item at `index` (newest-first list) at the top of the screen, newer messages below it. */
private suspend fun showAtTop(listState: LazyListState, index: Int) {
  listState.scrollToItem(index)
  val info = listState.layoutInfo
  val itemSize = info.visibleItemsInfo.firstOrNull { it.index == index }?.size ?: 0
  // In a reversed list a negative scroll moves toward the newest messages.
  listState.scrollBy(-(info.viewportEndOffset - info.viewportStartOffset - itemSize).toFloat())
}

/** A local file opens in another app; anything else is downloaded first (a second tap opens it). */
private fun openOrDownload(viewModel: MainViewModel, file: FileInfo, context: Context) {
  val path = file.transfer.path
  when {
    file.transfer.state == "local" && path != null -> openFile(context, File(path), file.mediaType)
    file.transfer.state == "downloading" -> {}
    else -> viewModel.act(ChatActions.download(file.conversation, file.message))
  }
}

/**
 * The way down: a round arrow while scrolled up, a "N new" pill once others' messages arrived
 * below; both go to the newest message.
 */
@Composable
private fun JumpDownButton(unseenCount: Int, onClick: () -> Unit) {
  if (unseenCount > 0) {
    val label = pluralStringResource(R.plurals.jump_down_new, unseenCount, unseenCount)
    Surface(
      onClick = onClick,
      shape = CircleShape,
      color = MaterialTheme.colorScheme.primaryContainer,
      contentColor = MaterialTheme.colorScheme.onPrimaryContainer,
      shadowElevation = 3.dp,
      modifier = Modifier.semantics { contentDescription = label },
    ) {
      Row(Modifier.padding(start = 10.dp, end = 14.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
        Icon(Icons.Filled.KeyboardArrowDown, contentDescription = null, modifier = Modifier.size(20.dp))
        val count = if (unseenCount > MAX_SHOWN_UNSEEN) "$MAX_SHOWN_UNSEEN+" else unseenCount.toString()
        Text(
          pluralStringResource(R.plurals.jump_down_unseen, unseenCount, count),
          style = MaterialTheme.typography.labelLarge,
          modifier = Modifier.padding(start = 4.dp),
        )
      }
    }
  } else {
    val label = stringResource(R.string.jump_down)
    SmallFloatingActionButton(onClick = onClick, modifier = Modifier.semantics { contentDescription = label }) {
      Icon(Icons.Filled.KeyboardArrowDown, contentDescription = null)
    }
  }
}

/**
 * The day of the topmost message as a pill over the feed while it scrolls, so a long scroll always
 * says where in time it is; it fades out shortly after the scrolling stops.
 */
@Composable
private fun FloatingDay(listState: LazyListState, items: List<FeedItem>, modifier: Modifier) {
  val maybeDay by remember(items) {
    derivedStateOf {
      // Reversed list: the topmost visible row has the highest index.
      listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index?.let { items.dayAt(it) }
    }
  }
  var isShown by remember { mutableStateOf(false) }
  LaunchedEffect(listState.isScrollInProgress) {
    if (listState.isScrollInProgress) {
      isShown = true
    } else {
      delay(DAY_PILL_LINGER_MS)
      isShown = false
    }
  }
  AnimatedVisibility(visible = isShown && maybeDay != null, enter = fadeIn(), exit = fadeOut(), modifier = modifier) {
    maybeDay?.let { DayPill(it, Modifier, isFloating = true) }
  }
}

/** The day the row at `index` (newest-first list) belongs to. */
private fun List<FeedItem>.dayAt(index: Int): LocalDate? =
  when (val item = getOrNull(index)) {
    is FeedItem.Day -> item.date
    is FeedItem.Entry -> Instant.ofEpochMilli(item.message.sentAt).atZone(ZoneId.systemDefault()).toLocalDate()
    // The unread line sits right above a message, which is the next newer row.
    FeedItem.Unread -> if (index > 0) dayAt(index - 1) else null
    null -> null
  }

/** "Today", "Yesterday" or the date, as a small centred pill. */
@Composable
private fun DayPill(date: LocalDate, modifier: Modifier, isFloating: Boolean = false) {
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
  Box(modifier, contentAlignment = Alignment.Center) {
    Surface(
      color = MaterialTheme.colorScheme.surfaceContainerHigh,
      shape = CircleShape,
      shadowElevation = if (isFloating) 2.dp else 0.dp,
    ) {
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
  Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp), verticalAlignment = Alignment.CenterVertically) {
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
    Icon(
      painterResource(R.drawable.ic_reply),
      contentDescription = null,
      tint = MaterialTheme.colorScheme.primary,
      modifier = Modifier
        .align(Alignment.CenterStart)
        .padding(start = 16.dp)
        .graphicsLayer { alpha = (offset.value / thresholdPx).coerceIn(0f, 1f) },
    )
    Box(Modifier.offset { IntOffset(offset.value.roundToInt(), 0) }) { content() }
  }
}

/** The colours of one bubble and of the text and markup inside it. */
private class BubbleColors(val container: Color, val content: Color, val muted: Color, val accent: Color, val markup: MarkupColors)

/**
 * Own bubbles in the accent (a deeper shade of it in dark mode, so white text stays readable),
 * others' in a neutral tone that stands off the feed's background in light and dark.
 */
@Composable
private fun bubbleColors(isMine: Boolean): BubbleColors {
  val scheme = MaterialTheme.colorScheme
  val extra = LocalTeamCraftColors.current
  return if (isMine) {
    val isDark = isSystemInDarkTheme()
    val container = if (isDark) lerp(scheme.primary, Color.Black, OWN_BUBBLE_DARKEN) else scheme.primary
    val content = if (isDark) Color.White else scheme.onPrimary
    BubbleColors(
      container = container,
      content = content,
      muted = content.copy(alpha = OWN_MUTED_ALPHA),
      accent = content,
      markup = MarkupColors(
        link = content,
        mention = content,
        mentionMe = content,
        mentionMeBackground = content.copy(alpha = OWN_HIGHLIGHT_ALPHA),
        codeBackground = content.copy(alpha = OWN_HIGHLIGHT_ALPHA),
      ),
    )
  } else {
    BubbleColors(
      container = scheme.surfaceContainerHighest,
      content = scheme.onSurface,
      muted = scheme.onSurfaceVariant,
      accent = scheme.primary,
      markup = MarkupColors(
        link = scheme.primary,
        mention = scheme.primary,
        mentionMe = extra.mentionText,
        mentionMeBackground = extra.mentionBackground,
        codeBackground = scheme.onSurface.copy(alpha = CODE_BACKGROUND_ALPHA),
      ),
    )
  }
}

private const val OWN_BUBBLE_DARKEN = 0.35f
private const val OWN_MUTED_ALPHA = 0.75f
private const val OWN_HIGHLIGHT_ALPHA = 0.18f
private const val CODE_BACKGROUND_ALPHA = 0.08f

@OptIn(ExperimentalFoundationApi::class, ExperimentalLayoutApi::class)
@Composable
private fun MessageRow(
  viewModel: MainViewModel,
  entry: FeedItem.Entry,
  maybeExtras: MessageExtras?,
  extras: Map<String, MessageExtras>,
  snapshot: ChatSnapshot?,
  look: FeedLook,
  onOpenThread: ((String) -> Unit)?,
  onOpenFile: (FileInfo) -> Unit,
  onLongPress: () -> Unit,
  maybeOnReply: ((Message) -> Unit)?,
  isHighlighted: Boolean,
  onOpenPermalink: (String) -> Unit,
) {
  val message = entry.message
  if (message.maybeSystemEvent != null || maybeExtras?.maybeWorkspaceEvent != null) {
    // Joins, renames and the like: a quiet centred line, not a bubble.
    Text(
      systemText(message, maybeExtras, snapshot),
      style = MaterialTheme.typography.labelMedium,
      fontStyle = FontStyle.Italic,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
      modifier = Modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 6.dp),
    )
    return
  }
  val haptics = LocalHapticFeedback.current
  val isMine = message.author == snapshot?.me?.id
  val replyLabel = stringResource(R.string.action_reply)
  val reactLabel = stringResource(R.string.react_quick, QUICK_REACTION)
  val moreLabel = stringResource(R.string.more)
  val showsSender = look.showsSenders && !isMine
  val maybeAuthor = snapshot?.user(message.author)
  // The bubble and the avatar share a row; reactions and the thread link go below, in line with the bubble.
  val indent = when {
    showsSender -> AVATAR_SIZE + AVATAR_GAP
    isMine -> 0.dp
    else -> 4.dp
  }
  val alignment = if (isMine) Alignment.End else Alignment.Start
  Column(
    Modifier
      .fillMaxWidth()
      .background(
        animateColorAsState(
          if (isHighlighted) MaterialTheme.colorScheme.primaryContainer else Color.Transparent,
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
      .padding(start = 8.dp, end = 8.dp, top = if (entry.isContinuation) 2.dp else 8.dp, bottom = 2.dp),
    horizontalAlignment = alignment,
  ) {
    Row(verticalAlignment = Alignment.Bottom) {
      if (showsSender) {
        // The avatar sits by the group's last bubble; the others keep its room so bubbles line up.
        Box(Modifier.size(AVATAR_SIZE)) {
          if (entry.isGroupEnd) SenderAvatar(maybeAuthor, snapshot?.name(message.author).orEmpty())
        }
        Spacer(Modifier.width(AVATAR_GAP))
      } else if (!isMine) {
        Spacer(Modifier.width(indent))
      }
      Box(Modifier.widthIn(max = look.bubbleMax)) {
        Bubble(
          viewModel = viewModel,
          entry = entry,
          maybeExtras = maybeExtras,
          extras = extras,
          snapshot = snapshot,
          look = look,
          isMine = isMine,
          showsName = showsSender && !entry.isContinuation,
          onOpenFile = onOpenFile,
          onOpenPermalink = onOpenPermalink,
        )
      }
    }
    Column(Modifier.padding(start = indent).widthIn(max = look.bubbleMax), horizontalAlignment = alignment) {
      BelowBubble(viewModel, message, maybeExtras, onOpenThread)
    }
  }
}

/** The bubble itself: name, forward and reply headers, body, attachments and the time. */
@Composable
private fun Bubble(
  viewModel: MainViewModel,
  entry: FeedItem.Entry,
  maybeExtras: MessageExtras?,
  extras: Map<String, MessageExtras>,
  snapshot: ChatSnapshot?,
  look: FeedLook,
  isMine: Boolean,
  showsName: Boolean,
  onOpenFile: (FileInfo) -> Unit,
  onOpenPermalink: (String) -> Unit,
) {
  val message = entry.message
  val colors = bubbleColors(isMine)
  val corners = bubbleCorners(isMine, entry.isContinuation, entry.isGroupEnd)
  val shape = RoundedCornerShape(corners.topStart, corners.topEnd, corners.bottomEnd, corners.bottomStart)
  val files = maybeExtras?.files.orEmpty()
  val images = files.filter { it.isImage && it.maybeThumbnail != null }
  val others = files - images.toSet()
  val isDeleted = maybeExtras?.isDeleted == true
  val hasText = !isDeleted && message.body.isNotBlank() && !isOnlyFileLabel(message.body, maybeExtras)
  Surface(color = colors.container, contentColor = colors.content, shape = shape) {
    Column(Modifier.padding(BUBBLE_MEDIA_PADDING)) {
      val textPadding = Modifier.padding(horizontal = BUBBLE_TEXT_PADDING_H - BUBBLE_MEDIA_PADDING)
      if (showsName) {
        Text(
          snapshot?.name(message.author).orEmpty(),
          style = MaterialTheme.typography.titleSmall,
          color = nameColor(snapshot?.user(message.author)),
          maxLines = 1,
          overflow = TextOverflow.Ellipsis,
          modifier = textPadding.padding(top = 4.dp),
        )
      }
      maybeExtras?.maybeForwarded?.let { forwarded ->
        Text(
          stringResource(R.string.forwarded_from, forwarded.authorName),
          style = MaterialTheme.typography.labelSmall,
          fontStyle = FontStyle.Italic,
          color = colors.muted,
          modifier = textPadding.padding(top = 4.dp),
        )
      }
      maybeExtras?.maybeReply?.let { reply ->
        ReplyQuote(reply, snapshot, extras, colors, Modifier.padding(top = 2.dp).fillMaxWidth())
      }
      if (images.isNotEmpty() && !isDeleted) {
        Box(Modifier.padding(top = if (showsName || maybeExtras?.maybeReply != null) 4.dp else 0.dp)) {
          if (images.size >= 2) ImageGrid(images, look.bubbleMax, onOpenFile) else ImagePreview(images.first(), look.bubbleMax, onOpenFile)
        }
      }
      Column(textPadding.padding(top = BUBBLE_TEXT_PADDING_V - BUBBLE_MEDIA_PADDING)) {
        when {
          isDeleted -> Text(
            stringResource(R.string.message_deleted),
            style = MaterialTheme.typography.bodyLarge,
            fontStyle = FontStyle.Italic,
            color = colors.muted,
          )
          hasText -> MessageBody(message.body, colors.content, colors.markup, look.names, onOpenPermalink)
        }
        if (!isDeleted) {
          others.forEach { file -> FileCard(file, colors, onOpenFile) }
          maybeExtras?.maybeLinkPreview?.let { LinkPreviewCard(it, colors) }
        }
        files.filter { it.transfer.isCancellable || it.transfer.state == "upload_failed" }.forEach { file ->
          UploadLine(viewModel, file, colors)
        }
        Footer(message, maybeExtras, colors, isMine)
      }
    }
  }
}

/** Reactions, read confirmations, "not sent" and the thread link, under the bubble. */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun BelowBubble(viewModel: MainViewModel, message: Message, maybeExtras: MessageExtras?, onOpenThread: ((String) -> Unit)?) {
  if (maybeExtras?.isSendFailed == true) NotSentLine(viewModel, message)
  if (message.reactions.isNotEmpty()) {
    FlowRow(
      horizontalArrangement = Arrangement.spacedBy(4.dp),
      verticalArrangement = Arrangement.spacedBy(4.dp),
      modifier = Modifier.padding(top = 2.dp),
    ) {
      for (reaction in message.reactions) {
        ReactionPill(reaction.emoji, reaction.count, reaction.reactedByMe) {
          viewModel.act(ChatActions.react(message.conversation, message.id, reaction.emoji))
        }
      }
    }
  }
  if (maybeExtras?.wantsAcknowledgement == true) {
    val count = maybeExtras.acknowledgedBy.size
    Text(
      pluralStringResource(R.plurals.acknowledged_by, count, count),
      style = MaterialTheme.typography.labelSmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = 4.dp, vertical = 2.dp),
    )
  }
  if (onOpenThread != null && message.replyCount > 0) {
    Text(
      pluralStringResource(R.plurals.replies, message.replyCount, message.replyCount),
      color = MaterialTheme.colorScheme.primary,
      style = MaterialTheme.typography.labelLarge,
      modifier = Modifier
        .clip(RoundedCornerShape(8.dp))
        .clickable { onOpenThread(message.id) }
        .padding(horizontal = 4.dp, vertical = 6.dp),
    )
  }
}

/** One reaction: the emoji and how many, outlined in the accent when the user is among them. */
@Composable
private fun ReactionPill(emoji: String, count: Int, isMine: Boolean, onClick: () -> Unit) {
  val scheme = MaterialTheme.colorScheme
  Surface(
    onClick = onClick,
    shape = CircleShape,
    color = if (isMine) scheme.primaryContainer else scheme.surfaceContainerHigh,
    border = BorderStroke(1.dp, if (isMine) scheme.primary else scheme.outlineVariant),
  ) {
    Text(
      "$emoji $count",
      style = MaterialTheme.typography.labelMedium,
      color = if (isMine) scheme.onPrimaryContainer else scheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = 8.dp, vertical = 3.dp),
    )
  }
}

/** Bottom-right inside the bubble: markers, "edited", the time and, for own messages, the state. */
@Composable
private fun ColumnScope.Footer(message: Message, maybeExtras: MessageExtras?, colors: BubbleColors, isMine: Boolean) {
  val context = LocalContext.current
  // A rejected message says "not sent" below instead of "sending…" forever.
  val isSending = message.isPending && maybeExtras?.isSendFailed != true
  Row(
    Modifier.align(Alignment.End).padding(top = 2.dp, bottom = 2.dp),
    verticalAlignment = Alignment.CenterVertically,
    horizontalArrangement = Arrangement.spacedBy(4.dp),
  ) {
    if (maybeExtras?.isPinned == true) {
      Icon(
        painterResource(R.drawable.ic_push_pin),
        contentDescription = stringResource(R.string.message_pinned),
        tint = colors.muted,
        modifier = Modifier.size(12.dp),
      )
    }
    when (maybeExtras?.priority) {
      "urgent" -> FooterLabel(stringResource(R.string.priority_urgent), if (isMine) colors.content else MaterialTheme.colorScheme.error, isBold = true)
      "important" -> FooterLabel(stringResource(R.string.priority_important), if (isMine) colors.content else MaterialTheme.colorScheme.tertiary, isBold = true)
    }
    if (message.isEdited) FooterLabel(stringResource(R.string.edited), colors.muted)
    FooterLabel(DateUtils.formatDateTime(context, message.sentAt, DateUtils.FORMAT_SHOW_TIME), colors.muted)
    when {
      !isMine -> {}
      isSending -> Icon(
        painterResource(R.drawable.ic_schedule),
        contentDescription = stringResource(R.string.pending),
        tint = colors.muted,
        modifier = Modifier.size(14.dp),
      )
      maybeExtras?.isSendFailed == true -> Icon(
        painterResource(R.drawable.ic_warning),
        contentDescription = stringResource(R.string.not_sent),
        tint = colors.content,
        modifier = Modifier.size(14.dp),
      )
      else -> Icon(Icons.Filled.Done, contentDescription = stringResource(R.string.message_sent), tint = colors.muted, modifier = Modifier.size(14.dp))
    }
  }
}

@Composable
private fun FooterLabel(text: String, color: Color, isBold: Boolean = false) {
  Text(
    text,
    style = MaterialTheme.typography.labelSmall,
    color = color,
    fontWeight = if (isBold) FontWeight.Bold else null,
    maxLines = 1,
  )
}

/** A sender's name in their own colour (the hue the core gives every user), readable in both themes. */
@Composable
private fun nameColor(maybeUser: User?): Color {
  val hue = (maybeUser?.avatarHue ?: 0).mod(360).toFloat()
  return Color.hsl(hue, NAME_SATURATION, if (isSystemInDarkTheme()) NAME_LIGHTNESS_DARK else NAME_LIGHTNESS_LIGHT)
}

/** A round avatar with the sender's initial on their hue. */
@Composable
private fun SenderAvatar(maybeUser: User?, name: String) {
  val hue = (maybeUser?.avatarHue ?: 0).mod(360).toFloat()
  Box(
    Modifier.size(AVATAR_SIZE).background(Color.hsl(hue, AVATAR_SATURATION, AVATAR_LIGHTNESS), CircleShape),
    contentAlignment = Alignment.Center,
  ) {
    Text(
      name.firstOrNull()?.uppercase() ?: "?",
      style = MaterialTheme.typography.labelLarge,
      color = Color.White,
      modifier = Modifier.clearAndSetSemantics {},
    )
  }
}

private const val NAME_SATURATION = 0.55f
private const val NAME_LIGHTNESS_LIGHT = 0.38f
private const val NAME_LIGHTNESS_DARK = 0.72f
private const val AVATAR_SATURATION = 0.45f
private const val AVATAR_LIGHTNESS = 0.45f

/** Inner padding of a bubble: media sit close to its edge, text has room. */
private val BUBBLE_MEDIA_PADDING = 3.dp
private val BUBBLE_TEXT_PADDING_H = 12.dp
private val BUBBLE_TEXT_PADDING_V = 6.dp

/** The core's body of a file message is the file names; with the file shown below, the body is noise. */
private fun isOnlyFileLabel(body: String, maybeExtras: MessageExtras?): Boolean {
  val files = maybeExtras?.files.orEmpty()
  return files.isNotEmpty() && body.trim() == files.joinToString(" ") { it.fileName }
}

/** An own message the server rejected: say so, and offer to send it again or drop it. */
@Composable
private fun NotSentLine(viewModel: MainViewModel, message: Message) {
  Row(verticalAlignment = Alignment.CenterVertically) {
    Text(
      stringResource(R.string.not_sent),
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

/**
 * An own file going up, as on desktop and web (#742): quiet while it goes up or retries by itself (a
 * small ring and cancel; how far, or "retrying", only for TalkBack); stopped: why, retry, cancel.
 */
@Composable
private fun UploadLine(viewModel: MainViewModel, file: FileInfo, colors: BubbleColors) {
  val context = LocalContext.current
  val cancel: () -> Unit = { viewModel.act(ChatActions.cancelUpload(file.conversation, file.message, file.fileId)) }
  Row(verticalAlignment = Alignment.CenterVertically) {
    when (file.transfer.state) {
      "uploading" -> {
        val transfer = file.transfer
        val label = if (transfer.isRetrying) {
          stringResource(R.string.file_upload_retrying)
        } else if (transfer.totalBytes >= LARGE_TRANSFER_BYTES) {
          "${Formatter.formatShortFileSize(context, transfer.doneBytes)} / ${Formatter.formatShortFileSize(context, transfer.totalBytes)}"
        } else {
          "${if (transfer.totalBytes > 0) transfer.doneBytes * 100 / transfer.totalBytes else 0} %"
        }
        val ring = Modifier.padding(start = 4.dp).size(16.dp).semantics { contentDescription = label }
        val maybeProgress = transfer.maybeProgress
        if (maybeProgress == null || transfer.isRetrying) {
          CircularProgressIndicator(ring, color = colors.content, strokeWidth = 2.dp)
        } else {
          CircularProgressIndicator(progress = { maybeProgress }, modifier = ring, color = colors.content, strokeWidth = 2.dp)
        }
        TextButton(onClick = cancel) { Text(stringResource(R.string.file_cancel_upload), color = colors.content) }
      }
      "upload_stalled" -> {
        val reason = if (file.transfer.isStorageUnavailable) R.string.file_upload_storage_unavailable else R.string.file_upload_stalled
        Text(
          stringResource(reason),
          color = colors.content,
          style = MaterialTheme.typography.labelSmall,
          modifier = Modifier.weight(1f, fill = false),
        )
        TextButton(onClick = { viewModel.act(ChatActions.retrySend(file.conversation, file.message)) }) {
          Text(stringResource(R.string.retry), color = colors.content)
        }
        TextButton(onClick = cancel) { Text(stringResource(R.string.file_cancel_upload), color = colors.content) }
      }
      else -> Text(stringResource(R.string.file_failed), color = colors.content, style = MaterialTheme.typography.labelSmall)
    }
  }
}

/**
 * The quoted message at the top of a reply's bubble: an accent bar, the author in the accent, one
 * line of the text and, when the quoted message has a picture, its thumbnail.
 */
@Composable
private fun ReplyQuote(reply: ReplyPreview, snapshot: ChatSnapshot?, extras: Map<String, MessageExtras>, colors: BubbleColors, modifier: Modifier) {
  val maybeImage = reply.maybeMessage?.let { extras[it] }?.files?.firstOrNull { it.isImage && it.maybeThumbnail != null }
  Row(
    modifier
      .clip(RoundedCornerShape(BUBBLE_CORNER_INNER))
      .background(colors.content.copy(alpha = QUOTE_BACKGROUND_ALPHA))
      .height(IntrinsicSize.Min),
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Box(Modifier.width(3.dp).fillMaxHeight().background(colors.accent))
    Column(Modifier.weight(1f, fill = false).padding(horizontal = 8.dp, vertical = 6.dp)) {
      Text(
        snapshot?.name(reply.author).orEmpty(),
        style = MaterialTheme.typography.labelLarge,
        color = colors.accent,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
      )
      Text(
        reply.excerpt,
        maxLines = 1,
        overflow = TextOverflow.Ellipsis,
        style = MaterialTheme.typography.bodyMedium,
        color = colors.muted,
      )
    }
    maybeImage?.let { image ->
      Box(Modifier.padding(4.dp).size(40.dp).clip(RoundedCornerShape(6.dp))) {
        Thumbnail(image, ContentScale.Crop)
      }
    }
  }
}

private val BUBBLE_CORNER_INNER = 12.dp
private val LINK_IMAGE_HEIGHT = 120.dp
private const val QUOTE_BACKGROUND_ALPHA = 0.08f

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

/**
 * A picture across the bubble's width at its own shape, at most [PREVIEW_MAX_HEIGHT] tall: portrait
 * photos stay whole instead of being cut to a strip (#315). Its size is known from the thumbnail's
 * header before the pixels are decoded, so the feed doesn't jump when they arrive.
 */
@Composable
private fun ImagePreview(file: FileInfo, bubbleMax: Dp, onOpen: (FileInfo) -> Unit) {
  val thumbnail = file.maybeThumbnail?.bytes ?: return
  val ratio = remember(thumbnail) { Pictures.aspectRatio(thumbnail) ?: DEFAULT_PREVIEW_RATIO }
    .coerceIn(MIN_PREVIEW_RATIO, MAX_PREVIEW_RATIO)
  val width = minOf(bubbleMax - BUBBLE_MEDIA_PADDING * 2, PREVIEW_MAX_HEIGHT * ratio)
  Box(
    Modifier
      .width(width)
      .aspectRatio(ratio)
      .clip(RoundedCornerShape(MEDIA_CORNER))
      .background(MaterialTheme.colorScheme.surfaceVariant)
      .clickable(onClickLabel = stringResource(R.string.file_open)) { onOpen(file) },
  ) {
    TransferOverlay(file) { Thumbnail(file, ContentScale.Crop) }
  }
}

/** A file that isn't a picture: its symbol, name and size; tap to download or open. */
@Composable
private fun FileCard(file: FileInfo, colors: BubbleColors, onOpen: (FileInfo) -> Unit) {
  val context = LocalContext.current
  Column(
    Modifier
      .padding(top = 4.dp)
      .clip(RoundedCornerShape(BUBBLE_CORNER_INNER))
      .background(colors.content.copy(alpha = QUOTE_BACKGROUND_ALPHA))
      .clickable(onClickLabel = stringResource(R.string.file_open)) { onOpen(file) }
      .padding(8.dp),
  ) {
    Row(verticalAlignment = Alignment.CenterVertically) {
      Box(
        Modifier.size(40.dp).background(colors.content.copy(alpha = QUOTE_BACKGROUND_ALPHA), RoundedCornerShape(8.dp)),
        contentAlignment = Alignment.Center,
      ) {
        Icon(painterResource(if (file.isImage) R.drawable.ic_image else R.drawable.ic_description), contentDescription = null, tint = colors.accent)
      }
      Column(Modifier.padding(start = 10.dp)) {
        Text(file.fileName, maxLines = 1, overflow = TextOverflow.Ellipsis, style = MaterialTheme.typography.bodyMedium, color = colors.content)
        Text(Formatter.formatShortFileSize(context, file.sizeBytes), style = MaterialTheme.typography.labelSmall, color = colors.muted)
      }
    }
    file.transfer.maybeProgress?.let { progress ->
      LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth().padding(top = 6.dp))
    }
    if (file.transfer.state == "failed") {
      Text(stringResource(R.string.file_failed), color = colors.content, style = MaterialTheme.typography.labelSmall)
    }
  }
}

/** Square tiles, two per row, across the bubble; a tap opens the picture like a single one. */
@Composable
private fun ImageGrid(images: List<FileInfo>, bubbleMax: Dp, onOpen: (FileInfo) -> Unit) {
  Column(Modifier.width(bubbleMax - BUBBLE_MEDIA_PADDING * 2), verticalArrangement = Arrangement.spacedBy(3.dp)) {
    for (row in images.chunked(2)) {
      Row(horizontalArrangement = Arrangement.spacedBy(3.dp)) {
        for (image in row) {
          Box(
            Modifier
              .weight(1f)
              .aspectRatio(1f)
              .clip(RoundedCornerShape(MEDIA_CORNER))
              .background(MaterialTheme.colorScheme.surfaceVariant)
              .clickable(onClickLabel = stringResource(R.string.file_open)) { onOpen(image) },
          ) {
            TransferOverlay(image) { Thumbnail(image, ContentScale.Crop) }
          }
        }
        // An odd last picture keeps its tile size.
        if (row.size == 1) Spacer(Modifier.weight(1f))
      }
    }
  }
}

/** A preview's picture, decoded in the background; the tile's own color stands in until then. */
@Composable
private fun Thumbnail(file: FileInfo, contentScale: ContentScale) {
  val maybeBitmap by rememberThumbnail(thumbnailKey(file), file.maybeThumbnail?.bytes, THUMBNAIL_DECODE_PX)
  maybeBitmap?.let { bitmap ->
    Image(bitmap, contentDescription = file.fileName, contentScale = contentScale, modifier = Modifier.fillMaxSize())
  }
}

/** The picture with an upload's or download's progress along its bottom edge. */
@Composable
private fun BoxScope.TransferOverlay(file: FileInfo, picture: @Composable () -> Unit) {
  picture()
  file.transfer.maybeProgress?.let { progress ->
    LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth().align(Alignment.BottomCenter))
  }
}

/** A link's preview inside the bubble: an accent bar, site, title, description and picture. */
@Composable
private fun LinkPreviewCard(preview: LinkPreview, colors: BubbleColors) {
  val context = LocalContext.current
  val maybeImage by rememberThumbnail("link:${preview.url}", preview.maybeImage?.bytes, THUMBNAIL_DECODE_PX)
  Row(
    Modifier
      .padding(top = 6.dp)
      .clip(RoundedCornerShape(BUBBLE_CORNER_INNER))
      .background(colors.content.copy(alpha = QUOTE_BACKGROUND_ALPHA))
      .clickable { openLink(context, preview.url) }
      .height(IntrinsicSize.Min),
  ) {
    Box(Modifier.width(3.dp).fillMaxHeight().background(colors.accent))
    Column(Modifier.padding(8.dp)) {
      maybeImage?.let {
        Image(
          it,
          contentDescription = null,
          modifier = Modifier.fillMaxWidth().height(LINK_IMAGE_HEIGHT).clip(RoundedCornerShape(8.dp)),
          contentScale = ContentScale.Crop,
        )
      }
      preview.maybeSiteName?.let { Text(it, style = MaterialTheme.typography.labelSmall, color = colors.muted, maxLines = 1) }
      preview.maybeTitle?.let {
        Text(it, style = MaterialTheme.typography.titleSmall, color = colors.content, maxLines = 2, overflow = TextOverflow.Ellipsis)
      }
      preview.maybeDescription?.let {
        Text(it, style = MaterialTheme.typography.bodySmall, color = colors.muted, maxLines = 3, overflow = TextOverflow.Ellipsis)
      }
    }
  }
}
