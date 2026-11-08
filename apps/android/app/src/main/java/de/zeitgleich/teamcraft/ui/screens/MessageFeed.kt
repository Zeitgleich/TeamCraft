package de.zeitgleich.teamcraft.ui.screens

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
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
import androidx.compose.foundation.layout.wrapContentWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListState
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Done
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
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
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.derivedStateOf
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.runtime.saveable.listSaver
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.runtime.snapshots.Snapshot
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.clipToBounds
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.lerp
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.layout.onSizeChanged
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.graphics.vector.rememberVectorPainter
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.CustomAccessibilityAction
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.customActions
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
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
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.first
import de.zeitgleich.teamcraft.R
import de.zeitgleich.teamcraft.ui.MessageLayout
import de.zeitgleich.teamcraft.ui.MessageLayoutPrefs
import de.zeitgleich.teamcraft.core.ChatActions
import de.zeitgleich.teamcraft.core.ChatSnapshot
import de.zeitgleich.teamcraft.core.FileInfo
import de.zeitgleich.teamcraft.core.HistoryGap
import de.zeitgleich.teamcraft.core.LinkPreview
import de.zeitgleich.teamcraft.core.Message
import de.zeitgleich.teamcraft.core.MessageExtras
import de.zeitgleich.teamcraft.core.ReplyAttachmentPreview
import de.zeitgleich.teamcraft.core.ReplyPreview
import de.zeitgleich.teamcraft.core.User
import de.zeitgleich.teamcraft.core.UnseenReactions
import de.zeitgleich.teamcraft.ui.avatarColorOfHue
import de.zeitgleich.teamcraft.ui.OlderState
import de.zeitgleich.teamcraft.ui.DividerOnScreen
import de.zeitgleich.teamcraft.ui.FeedItem
import de.zeitgleich.teamcraft.ui.FeedScrollAction
import de.zeitgleich.teamcraft.ui.FeedScrollEvent
import de.zeitgleich.teamcraft.ui.FeedScrollState
import de.zeitgleich.teamcraft.ui.FirstUnread
import de.zeitgleich.teamcraft.ui.GapRequest
import de.zeitgleich.teamcraft.ui.MainViewModel
import de.zeitgleich.teamcraft.ui.PlacedGap
import de.zeitgleich.teamcraft.ui.ServerHistory
import de.zeitgleich.teamcraft.ui.gapEdge
import de.zeitgleich.teamcraft.ui.gapToFill
import de.zeitgleich.teamcraft.ui.MarkupColors
import de.zeitgleich.teamcraft.ui.MentionNames
import de.zeitgleich.teamcraft.ui.AUTO_FETCH_SETTLE_MS
import de.zeitgleich.teamcraft.ui.AnimationBadge
import de.zeitgleich.teamcraft.ui.AnimationButton
import de.zeitgleich.teamcraft.ui.AnimationRead
import de.zeitgleich.teamcraft.ui.AnimationView
import de.zeitgleich.teamcraft.ui.FeedMotion
import de.zeitgleich.teamcraft.ui.LocalFeedMotion
import de.zeitgleich.teamcraft.ui.MAX_AUTO_ANIMATION_BYTES
import de.zeitgleich.teamcraft.ui.MediaPrefs
import de.zeitgleich.teamcraft.ui.PictureTarget
import de.zeitgleich.teamcraft.ui.PlayChoice
import de.zeitgleich.teamcraft.ui.animationFetchBudget
import de.zeitgleich.teamcraft.ui.animationLabel
import de.zeitgleich.teamcraft.ui.isAnimationType
import de.zeitgleich.teamcraft.ui.isAppResumed
import de.zeitgleich.teamcraft.ui.mayBeAnimated
import de.zeitgleich.teamcraft.ui.rememberAnimation
import de.zeitgleich.teamcraft.ui.rememberMotionReduced
import de.zeitgleich.teamcraft.ui.wantsToMove
import de.zeitgleich.teamcraft.ui.Pictures
import de.zeitgleich.teamcraft.ui.SourceAvatar
import de.zeitgleich.teamcraft.ui.UserAvatar
import de.zeitgleich.teamcraft.ui.THUMBNAIL_DECODE_PX
import de.zeitgleich.teamcraft.ui.bubbleCorners
import de.zeitgleich.teamcraft.ui.dayShowsYear
import de.zeitgleich.teamcraft.ui.decideFeedScroll
import de.zeitgleich.teamcraft.ui.feedItems
import de.zeitgleich.teamcraft.ui.isNearBottom
import de.zeitgleich.teamcraft.ui.localizedText
import de.zeitgleich.teamcraft.ui.newerMessages
import de.zeitgleich.teamcraft.ui.pinnedDay
import de.zeitgleich.teamcraft.ui.reactionsInView
import de.zeitgleich.teamcraft.ui.rememberThumbnail
import de.zeitgleich.teamcraft.ui.rowOf
import de.zeitgleich.teamcraft.ui.showsSenders
import de.zeitgleich.teamcraft.ui.SystemLine
import de.zeitgleich.teamcraft.ui.systemLine
import de.zeitgleich.teamcraft.ui.thumbnailKey
import de.zeitgleich.teamcraft.ui.theme.LocalTeamCraftColors
import uniffi.teamcraft_ffi.AlbumLayout
import uniffi.teamcraft_ffi.albumLayout
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

/** Where the pinned day's pill sits below the top of the feed. */
private val DAY_PIN_TOP = 8.dp

/** The vertical padding around a day divider's pill in its row. */
private val DAY_DIVIDER_PADDING = 8.dp

/** The pinned pill is a little translucent, so the text scrolling under it shows through. */
private const val PINNED_PILL_ALPHA = 0.92f

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

/** The list layout (#1488): large avatars, generous room between senders, as on the desktop. */
private val LIST_AVATAR_SIZE = 40.dp
private val LIST_AVATAR_GAP = 12.dp
private val LIST_PADDING_H = 16.dp
private val LIST_GROUP_GAP = 12.dp
private val LIST_MAX_WIDTH = 640.dp
private val MEDIA_CORNER = 14.dp

/**
 * Rows above the oldest one on screen at which the feed asks for older messages: a few screens
 * ahead, so a reader scrolling back doesn't meet the end of what is loaded (issue #1058).
 */
private const val OLDER_AHEAD_ROWS = 40

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
  /** Answers one picture of an album. */
  maybeOnReplyImage: ((FileInfo) -> Unit)? = null,
  /** A long press on a picture of an album. */
  onLongPressImage: (FileInfo) -> Unit = {},
  /** Opens the viewer from outside the feed (the picture's action sheet). */
  maybeOpenPicture: PictureTarget? = null,
  onPictureOpened: () -> Unit = {},
  modifier: Modifier = Modifier,
  maybeFocusMessage: String? = null,
  onOpenPermalink: (String) -> Unit = {},
  /** Opens another conversation: the room a call moved into (issue #1290). */
  onOpenConversation: (String) -> Unit = {},
  /** Whether the user may react here; without it the reactions stay readable but don't toggle. */
  canReact: Boolean = true,
  older: OlderMessages = OlderMessages.None,
  /** Others' reactions to the user's messages not seen yet: the heart button (issue #1009). */
  unseenReactions: UnseenReactions = UnseenReactions(),
  /** These messages' reactions were on screen, or the user jumped to them. */
  onReactionsSeen: (List<String>) -> Unit = {},
  /** Holes in the history among [messages] (issue #1438), drawn where they are. */
  gaps: List<PlacedGap> = emptyList(),
  /** The reader came near a hole, or asked again after a failure: a page of it from that side. */
  onFillGap: (GapRequest) -> Unit = {},
) {
  // Newest first in a reversed list: the bottom stays anchored when the keyboard opens or a picture
  // above grows, and the rows keep their place by key (the message ID) when new ones arrive.
  val items = remember(messages, firstUnread, gaps) {
    feedItems(messages, firstUnread.maybeMessageId, gaps = gaps).asReversed()
  }
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
  // Saveable: a rotation keeps the viewer open (the conversation, message and file it opened on).
  var maybeViewing by rememberSaveable { mutableStateOf<List<String>?>(null) }
  var maybeVideo by rememberSaveable { mutableStateOf<List<String>?>(null) }
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
  PageBack(listState, items.size, older)
  FillGaps(listState, items, onFillGap)
  val onSeen by rememberUpdatedState(onReactionsSeen)
  // Told once per message while it waits; a message whose reactions were seen and that gets a new
  // one later waits again.
  val reported = remember { mutableSetOf<String>() }
  LaunchedEffect(listState, items, unseenReactions) {
    reported.retainAll(unseenReactions.messages.toSet())
    if (unseenReactions.messages.isEmpty()) return@LaunchedEffect
    snapshotFlow { listState.layoutInfo.visibleItemsInfo.map { it.index } }.collect { visible ->
      val seen = reactionsInView(items, visible, unseenReactions.messages, reported)
      if (seen.isNotEmpty()) {
        reported.addAll(seen)
        onSeen(seen)
      }
    }
  }
  // The message the heart button goes to, until its row is loaded.
  var maybeReactionTarget by remember { mutableStateOf<String?>(null) }
  LaunchedEffect(maybeReactionTarget, items, older.hasOlder, older.isLoading) {
    val target = maybeReactionTarget ?: return@LaunchedEffect
    val maybeRow = rowOf(items, target)
    when {
      maybeRow != null -> {
        maybeReactionTarget = null
        maybeHighlighted = target
        showAtTop(listState, maybeRow)
      }
      older.hasOlder -> if (!older.isLoading) older.onNearOldest()
      else -> maybeReactionTarget = null
    }
  }
  val context = LocalContext.current
  val onOpenFile: (FileInfo) -> Unit = remember(viewModel, context) {
    { file ->
      // Pictures open in the viewer at once (it shows the thumbnail and fetches the original).
      when {
        file.isImage && file.maybeThumbnail != null -> maybeViewing = PictureTarget.of(file).toList()
        file.opensInVideoPlayer -> maybeVideo = PictureTarget.of(file).toList()
        else -> openOrDownload(viewModel, file, context)
      }
    }
  }
  val mediaPrefs = remember(context) { MediaPrefs.prefs(context) }
  val animatedImages by mediaPrefs.animatedImages.collectAsState()
  val isMotionReduced = rememberMotionReduced()
  val feedMotion = FeedMotion(
    mode = animatedImages.effective(isMotionReduced),
    isCovered = maybeViewing != null || maybeVideo != null,
    fetchesNow = mediaPrefs::fetchesOriginalNow,
    fetch = { file -> viewModel.act(ChatActions.downloadAttachment(file.conversation, file.message, file.fileId)) },
  )
  // Once per snapshot, not per row: mentions are matched against every user's name.
  val names = remember(snapshot) {
    MentionNames(snapshot?.users.orEmpty().map { it.username.lowercase() }.toSet(), snapshot?.me?.username.orEmpty())
  }
  val showsSenders = showsSenders(messages.firstOrNull()?.let { snapshot?.conversation(it.conversation)?.kind })
  val messageLayout by remember { MessageLayoutPrefs.of(context) }.layout.collectAsState()
  val isList = messageLayout == MessageLayout.List
  BoxWithConstraints(modifier.fillMaxSize()) {
    val bubbleMax = if (isList) {
      minOf(maxWidth - LIST_AVATAR_SIZE - LIST_AVATAR_GAP - LIST_PADDING_H * 2, LIST_MAX_WIDTH)
    } else {
      minOf(maxWidth * BUBBLE_WIDTH_SHARE, BUBBLE_MAX_WIDTH)
    }
    val look = FeedLook(bubbleMax = bubbleMax, showsSenders = showsSenders, names = names, isList = isList)
    CompositionLocalProvider(LocalFeedMotion provides feedMotion) {
      LazyColumn(Modifier.fillMaxSize(), state = listState, reverseLayout = true) {
        // Rows of one kind reuse each other's layout when they scroll in.
        items(items, key = { it.key }, contentType = { it.contentType }) { item ->
          when (item) {
            is FeedItem.Day -> DayPill(item.date, Modifier.fillMaxWidth().padding(vertical = DAY_DIVIDER_PADDING))
            FeedItem.Unread -> UnreadDivider()
            is FeedItem.Gap -> GapRow(item.gap) {
              // From the side the reader is on now.
              val row = items.indexOf(item)
              val topVisible = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: row
              onFillGap(GapRequest(item.gap, gapEdge(row, topVisible)))
            }
            // A placeholder (issue #888) has nothing to reply to or act on yet.
            is FeedItem.Entry -> SwipeToReply(item.message, maybeOnReply.takeIf { item.message.maybePlaceholder == null }) {
              MessageRow(
                viewModel = viewModel,
                entry = item,
                maybeExtras = extras[item.message.id],
                extras = extras,
                snapshot = snapshot,
                look = look,
                onOpenThread = onOpenThread,
                onOpenFile = onOpenFile,
                onOpenPicture = { target -> maybeViewing = target.toList() },
                onLongPress = { onLongPress(item.message) },
                onLongPressImage = onLongPressImage,
                maybeOnReply = maybeOnReply,
                isHighlighted = item.message.id == maybeHighlighted,
                onOpenPermalink = onOpenPermalink,
                onOpenConversation = onOpenConversation,
                canReact = canReact,
              )
            }
          }
        }
        // Above the oldest row: seen only when the reader is faster than the core.
        if (older.hasOlder && older.isLoading) item(key = OLDER_LOADING_KEY) { OlderLoadingRow() }
        if (older.isFailed) item(key = OLDER_FAILED_KEY) { OlderFailedRow(older.onRetry) }
      }
    }
    FloatingDay(listState, items, Modifier.align(Alignment.TopCenter))
    Column(
      Modifier.align(Alignment.BottomEnd).padding(16.dp),
      horizontalAlignment = Alignment.End,
      verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
      AnimatedVisibility(visible = unseenReactions.count > 0 && unseenReactions.messages.isNotEmpty(), enter = fadeIn(), exit = fadeOut()) {
        ReactionJumpButton(unseenReactions.count) {
          unseenReactions.messages.firstOrNull()?.let { oldest ->
            reported.add(oldest)
            onSeen(listOf(oldest))
            maybeReactionTarget = oldest
          }
        }
      }
      AnimatedVisibility(
        visible = (showsJumpDown || scrollState.unseenCount > 0) && items.isNotEmpty(),
        enter = fadeIn(),
        exit = fadeOut(),
      ) {
        JumpDownButton(scrollState.unseenCount) {
          decide(FeedScrollEvent.ReachedBottom, isNearBottom = true)
          scope.launch { listState.animateScrollToItem(0) }
        }
      }
    }
  }
  LaunchedEffect(maybeOpenPicture) {
    maybeOpenPicture?.let { target ->
      maybeViewing = target.toList()
      onPictureOpened()
    }
  }
  maybeVideo?.let(PictureTarget::fromList)?.let { target ->
    VideoViewer(viewModel, target, onDismiss = { maybeVideo = null })
  }
  maybeViewing?.let(PictureTarget::fromList)?.let { target ->
    ImageViewer(
      viewModel,
      snapshot,
      target,
      onDismiss = { maybeViewing = null },
      maybeOnReply = maybeOnReplyImage?.let { reply ->
        { file: FileInfo ->
          maybeViewing = null
          reply(file)
        }
      },
    )
  }
}

/** Whether a feed can page further back (issue #1058); threads and searches show all they have. */
class OlderMessages(state: OlderState, val onRetry: () -> Unit, val onNearOldest: () -> Unit) {
  val hasOlder = state.hasOlder
  val isLoading = state.isLoading
  val isFailed = state.isFailed

  companion object {
    val None = OlderMessages(OlderState(hasOlder = false, isLoading = false, isFailed = false), onRetry = {}) {}
  }
}

private const val OLDER_LOADING_KEY = "older-loading"
private const val OLDER_FAILED_KEY = "older-failed"

/** Asks for older messages once the oldest loaded row comes within [OLDER_AHEAD_ROWS] of the screen. */
@Composable
private fun PageBack(listState: LazyListState, rowCount: Int, older: OlderMessages) {
  val onNearOldest by rememberUpdatedState(older.onNearOldest)
  // Restarted when rows arrive or a load ends: a reader still near the top asks for the next page.
  LaunchedEffect(listState, rowCount, older.hasOlder, older.isLoading) {
    if (!older.hasOlder || older.isLoading) return@LaunchedEffect
    snapshotFlow {
      // Reversed list: the topmost visible row has the highest index.
      val topmost = listState.layoutInfo.visibleItemsInfo.lastOrNull()?.index ?: return@snapshotFlow false
      topmost >= rowCount - 1 - OLDER_AHEAD_ROWS
    }.first { it }
    onNearOldest()
  }
}

/**
 * Asks for a page of a hole in the history once its row comes within [OLDER_AHEAD_ROWS] of the
 * screen ([gapToFill]), like older history: not at the bottom of a list that scrolls, so opening a
 * conversation costs no extra request (the catch-up reads the hole by itself), and again after every
 * answer while the reader is still near.
 */
@Composable
private fun FillGaps(listState: LazyListState, items: List<FeedItem>, onFillGap: (GapRequest) -> Unit) {
  val fill by rememberUpdatedState(onFillGap)
  val gapRows = remember(items) {
    items.withIndex().mapNotNull { (index, item) -> (item as? FeedItem.Gap)?.let { index to it.gap } }
  }
  // Restarted when the rows change: a page arrived, or the answer changed the hole's status.
  LaunchedEffect(listState, gapRows) {
    if (gapRows.none { (_, gap) -> gap.status == ServerHistory.AVAILABLE }) return@LaunchedEffect
    val request = snapshotFlow {
      val visible = listState.layoutInfo.visibleItemsInfo
      val isAtBottom = listState.firstVisibleItemIndex == 0 && listState.firstVisibleItemScrollOffset <= AT_BOTTOM_SLOP_PX
      val fits = !listState.canScrollForward && !listState.canScrollBackward
      if (visible.isEmpty() || (isAtBottom && !fits)) {
        null
      } else {
        // Reversed list: the bottommost visible row has the lowest index.
        gapToFill(gapRows, visible.first().index, visible.last().index, OLDER_AHEAD_ROWS)
      }
    }.filterNotNull().first()
    fill(request)
  }
}

/**
 * Where history this device hasn't read yet lies (issue #1438): a quiet loading line while the
 * catch-up or a request reads it, the failure with "Try again" when the last request failed.
 */
@Composable
private fun GapRow(gap: HistoryGap, onRetry: () -> Unit) {
  Row(
    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 12.dp),
    horizontalArrangement = Arrangement.Center,
    verticalAlignment = Alignment.CenterVertically,
  ) {
    if (gap.status == ServerHistory.FAILED) {
      Text(
        stringResource(R.string.history_gap_failed),
        Modifier.weight(1f, fill = false),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
      )
      TextButton(onClick = onRetry) { Text(stringResource(R.string.older_history_retry)) }
    } else {
      CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 2.dp)
      Text(
        stringResource(R.string.history_gap_loading),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(start = 8.dp),
      )
    }
  }
}

@Composable
private fun OlderLoadingRow() {
  val label = stringResource(R.string.loading_older_messages)
  Box(Modifier.fillMaxWidth().padding(vertical = 12.dp).semantics { contentDescription = label }, contentAlignment = Alignment.Center) {
    CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp)
  }
}

/** Older messages could not be read: says so where they would be, and the next connection asks again. */
@Composable
private fun OlderFailedRow(onRetry: () -> Unit) {
  Row(
    Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
    horizontalArrangement = Arrangement.Center,
    verticalAlignment = Alignment.CenterVertically,
  ) {
    Text(
      stringResource(R.string.older_history_failed),
      Modifier.weight(1f, fill = false),
      style = MaterialTheme.typography.bodySmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    TextButton(onClick = onRetry) { Text(stringResource(R.string.older_history_retry)) }
  }
}

/** What every bubble of a feed shares. */
private class FeedLook(
  val bubbleMax: Dp,
  val showsSenders: Boolean,
  val names: MentionNames,
  /** The desktop's list instead of bubbles (#1488): every message on the left, under its sender. */
  val isList: Boolean = false,
)

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
 * The way to others' reactions to the user's messages that weren't seen yet: a heart with their
 * count, above the way down; it goes to the oldest such message.
 */
@Composable
private fun ReactionJumpButton(count: Int, onClick: () -> Unit) {
  val label = pluralStringResource(R.plurals.jump_to_reaction, count, count)
  SmallFloatingActionButton(onClick = onClick, modifier = Modifier.semantics { contentDescription = label }) {
    BadgedBox(badge = { Badge { Text(if (count > MAX_SHOWN_UNSEEN) "$MAX_SHOWN_UNSEEN+" else count.toString()) } }) {
      Icon(Icons.Filled.FavoriteBorder, contentDescription = null)
    }
  }
}

/**
 * The day of the topmost message pinned over the feed while it scrolls (#1046), so a long scroll
 * always says where in time it is; the next day's divider pushes it up and out ([pinnedDay]). It
 * fades out shortly after the scrolling stops. Not a `stickyHeader`: in this reversed list a header
 * would pin to the bottom, below the days it heads.
 */
@Composable
private fun FloatingDay(listState: LazyListState, items: List<FeedItem>, modifier: Modifier) {
  val density = LocalDensity.current
  val pinTop = with(density) { DAY_PIN_TOP.roundToPx() }
  val dividerPadding = with(density) { DAY_DIVIDER_PADDING.roundToPx() }
  var pillHeight by remember { mutableIntStateOf(0) }
  // From the list's own layout pass: nothing is measured again per frame.
  val maybePinned by remember(items) {
    derivedStateOf {
      val layout = listState.layoutInfo
      // Reversed list: offsets count from the bottom, and the topmost visible row has the highest index.
      val height = layout.viewportSize.height
      val dividers = layout.visibleItemsInfo.mapNotNull { info ->
        (items.getOrNull(info.index) as? FeedItem.Day)?.let { DividerOnScreen(it.date, height - info.offset - info.size + dividerPadding) }
      }
      val topmostDay = layout.visibleItemsInfo.lastOrNull()?.index?.let { index ->
        // A divider at the very top belongs to the rows below it; the day above is the older one.
        if (items.getOrNull(index) is FeedItem.Day) items.dayAt(index + 1) else items.dayAt(index)
      }
      pinnedDay(dividers, topmostDay, pinTop, pillHeight)
    }
  }
  val maybeDay by remember { derivedStateOf { maybePinned?.date } }
  var isShown by remember { mutableStateOf(false) }
  LaunchedEffect(listState.isScrollInProgress) {
    if (listState.isScrollInProgress) {
      isShown = true
    } else {
      delay(DAY_PILL_LINGER_MS)
      isShown = false
    }
  }
  AnimatedVisibility(
    visible = isShown && maybeDay != null,
    enter = fadeIn(),
    exit = fadeOut(),
    modifier = modifier.fillMaxWidth().clipToBounds(),
  ) {
    maybeDay?.let { day ->
      DayPill(
        day,
        Modifier
          // Read while drawing only: the push moves the layer without composing again.
          .graphicsLayer { translationY = (maybePinned?.push ?: 0).toFloat() }
          .padding(top = DAY_PIN_TOP, bottom = 4.dp)
          .onSizeChanged { pillHeight = it.height },
        isFloating = true,
      )
    }
  }
}

/** The day the row at `index` (newest-first list) belongs to. */
private fun List<FeedItem>.dayAt(index: Int): LocalDate? =
  when (val item = getOrNull(index)) {
    is FeedItem.Day -> item.date
    is FeedItem.Entry -> Instant.ofEpochMilli(item.message.sentAt).atZone(ZoneId.systemDefault()).toLocalDate()
    // The unread line sits right above a message, which is the next newer row; a hole is read with the rows below it.
    FeedItem.Unread, is FeedItem.Gap -> if (index > 0) dayAt(index - 1) else null
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
        if (dayShowsYear(date, today)) DateUtils.FORMAT_SHOW_YEAR else DateUtils.FORMAT_NO_YEAR
      DateUtils.formatDateTime(context, millis, flags)
    }
  }
  Box(modifier, contentAlignment = Alignment.Center) {
    Surface(
      color = MaterialTheme.colorScheme.surfaceContainerHigh.copy(alpha = if (isFloating) PINNED_PILL_ALPHA else 1f),
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
private class BubbleColors(val container: Color, val content: Color, val muted: Color, val accent: Color, val markup: MarkupColors) {
  fun withContainer(color: Color) = BubbleColors(color, content, muted, accent, markup)
}

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
  onOpenPicture: (PictureTarget) -> Unit,
  onLongPress: () -> Unit,
  onLongPressImage: (FileInfo) -> Unit,
  maybeOnReply: ((Message) -> Unit)?,
  isHighlighted: Boolean,
  onOpenPermalink: (String) -> Unit,
  onOpenConversation: (String) -> Unit,
  canReact: Boolean,
) {
  val message = entry.message
  val maybeSystemLine = systemLine(message, maybeExtras) { snapshot?.user(it)?.displayName.orEmpty() }
  if (maybeSystemLine != null) {
    // Joins, renames, who added whom: a quiet centred line in the desktop and web wording (#769).
    Text(
      maybeSystemLine.localizedText(),
      style = MaterialTheme.typography.labelMedium,
      fontStyle = FontStyle.Italic,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      textAlign = TextAlign.Center,
      modifier = Modifier.fillMaxWidth().padding(horizontal = 32.dp, vertical = 6.dp),
    )
    // The room a call moved into, where its summary lands (issue #1290).
    (maybeSystemLine as? SystemLine.CallMoved)?.maybeRoomId?.let { room ->
      TextButton(
        onClick = { onOpenConversation(room) },
        modifier = Modifier.fillMaxWidth().wrapContentWidth(Alignment.CenterHorizontally),
      ) {
        Text(stringResource(R.string.event_call_moved_open), style = MaterialTheme.typography.labelMedium)
      }
    }
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
  val reactQuickly: () -> Unit = { viewModel.act(ChatActions.react(message.conversation, message.id, QUICK_REACTION)) }
  // Not readable yet (issue #888): no reactions, replies or actions until it is.
  val isPlaceholder = message.maybePlaceholder != null
  val rowModifier = Modifier
    .fillMaxWidth()
    .background(
      animateColorAsState(
        if (isHighlighted) MaterialTheme.colorScheme.primaryContainer else Color.Transparent,
        label = "highlight",
      ).value,
    )
    .combinedClickable(
      onClick = {},
      // Without the right to react, a tap doesn't wait to tell a double tap apart either.
      onDoubleClick = reactQuickly.takeIf { canReact && !isPlaceholder },
      onLongClick = {
        haptics.performHapticFeedback(HapticFeedbackType.LongPress)
        onLongPress()
      }.takeIf { !isPlaceholder },
    )
    .semantics {
      if (isPlaceholder) return@semantics
      customActions = listOfNotNull(
        maybeOnReply?.let { reply -> CustomAccessibilityAction(replyLabel) { reply(message); true } },
        CustomAccessibilityAction(reactLabel) { reactQuickly(); true }.takeIf { canReact },
        CustomAccessibilityAction(moreLabel) { onLongPress(); true },
      )
    }
  // The long-press menu has every reaction; the pill after the reactions opens it too.
  val maybeOnAddReaction = onLongPress.takeIf { canReact && !isPlaceholder }
  if (look.isList) {
    ListMessageRow(
      viewModel, entry, maybeExtras, extras, snapshot, look, isMine, onOpenThread, onOpenFile, onOpenPicture,
      onLongPressImage, onOpenPermalink, canReact, maybeOnAddReaction, rowModifier,
    )
    return
  }
  Column(
    rowModifier.padding(start = 8.dp, end = 8.dp, top = if (entry.isContinuation) 2.dp else 8.dp, bottom = 2.dp),
    horizontalAlignment = alignment,
  ) {
    Row(verticalAlignment = Alignment.Bottom) {
      if (showsSender) {
        // The avatar sits by the group's last bubble; the others keep its room so bubbles line up.
        Box(Modifier.size(AVATAR_SIZE)) {
          if (entry.isGroupEnd) {
            SenderAvatar(maybeAuthor, message.maybeSource?.name ?: snapshot?.name(message.author).orEmpty(), message)
          }
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
          onOpenPicture = onOpenPicture,
          onLongPressImage = onLongPressImage,
          onOpenPermalink = onOpenPermalink,
        )
      }
    }
    Column(Modifier.padding(start = indent).widthIn(max = look.bubbleMax), horizontalAlignment = alignment) {
      BelowBubble(viewModel, message, maybeExtras, onOpenThread, canReact, maybeOnAddReaction)
    }
  }
}

/**
 * A message in the list layout (#1488): the sender's avatar, name and time on one line at the
 * start of a group, the message below without a bubble, everything on the left.
 */
@Composable
private fun ListMessageRow(
  viewModel: MainViewModel,
  entry: FeedItem.Entry,
  maybeExtras: MessageExtras?,
  extras: Map<String, MessageExtras>,
  snapshot: ChatSnapshot?,
  look: FeedLook,
  isMine: Boolean,
  onOpenThread: ((String) -> Unit)?,
  onOpenFile: (FileInfo) -> Unit,
  onOpenPicture: (PictureTarget) -> Unit,
  onLongPressImage: (FileInfo) -> Unit,
  onOpenPermalink: (String) -> Unit,
  canReact: Boolean,
  maybeOnAddReaction: (() -> Unit)?,
  modifier: Modifier,
) {
  val message = entry.message
  val maybeAuthor = snapshot?.user(message.author)
  // A source's name always says it's a bot.
  val name = message.maybeSource?.let { stringResource(R.string.source_sender, it.name) }
    ?: snapshot?.name(message.author).orEmpty()
  Row(
    modifier.padding(
      start = LIST_PADDING_H,
      end = LIST_PADDING_H,
      top = if (entry.isContinuation) 2.dp else LIST_GROUP_GAP,
      bottom = 2.dp,
    ),
  ) {
    Box(Modifier.width(LIST_AVATAR_SIZE)) {
      if (!entry.isContinuation) SenderAvatar(maybeAuthor, snapshot?.name(message.author).orEmpty(), message, LIST_AVATAR_SIZE)
    }
    Spacer(Modifier.width(LIST_AVATAR_GAP))
    Column(Modifier.weight(1f)) {
      if (!entry.isContinuation) SenderLine(name, message.sentAt, nameColor(maybeAuthor))
      Box(Modifier.widthIn(max = look.bubbleMax)) {
        Bubble(
          viewModel = viewModel,
          entry = entry,
          maybeExtras = maybeExtras,
          extras = extras,
          snapshot = snapshot,
          look = look,
          isMine = isMine,
          showsName = false,
          onOpenFile = onOpenFile,
          onOpenPicture = onOpenPicture,
          onLongPressImage = onLongPressImage,
          onOpenPermalink = onOpenPermalink,
        )
      }
      BelowBubble(viewModel, message, maybeExtras, onOpenThread, canReact, maybeOnAddReaction)
    }
  }
}

/** The name in bold and the time after it, on one line. */
@Composable
private fun SenderLine(name: String, sentAt: Long, color: Color) {
  val context = LocalContext.current
  Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
    Text(
      name,
      style = MaterialTheme.typography.titleSmall,
      fontWeight = FontWeight.Bold,
      color = color,
      maxLines = 1,
      overflow = TextOverflow.Ellipsis,
      modifier = Modifier.weight(1f, fill = false),
    )
    Text(
      DateUtils.formatDateTime(context, sentAt, DateUtils.FORMAT_SHOW_TIME),
      style = MaterialTheme.typography.labelSmall,
      color = MaterialTheme.colorScheme.onSurfaceVariant,
      maxLines = 1,
    )
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
  onOpenPicture: (PictureTarget) -> Unit,
  onLongPressImage: (FileInfo) -> Unit,
  onOpenPermalink: (String) -> Unit,
) {
  val message = entry.message
  // The list has no bubble: the text sits on the feed in the others' colours, mine included.
  val colors = if (look.isList) bubbleColors(isMine = false).withContainer(Color.Transparent) else bubbleColors(isMine)
  val corners = bubbleCorners(isMine, entry.isContinuation, entry.isGroupEnd)
  val shape = RoundedCornerShape(corners.topStart, corners.topEnd, corners.bottomEnd, corners.bottomStart)
  val files = maybeExtras?.files.orEmpty()
  val images = files.filter { it.isImage && it.maybeThumbnail != null }
  val videos = files.filter { it.isVideo && it.maybeThumbnail != null }
  val others = files - images.toSet() - videos.toSet()
  val isDeleted = maybeExtras?.isDeleted == true
  val hasText = !isDeleted && message.body.isNotBlank() && !isOnlyFileLabel(message.body, maybeExtras)
  val hasTransfers = files.any { it.transfer.isCancellable || it.transfer.state == "upload_failed" }
  val isPicturesOnly = images.isNotEmpty() && !isDeleted && !hasText && !showsName && others.isEmpty() && videos.isEmpty() &&
    message.maybePlaceholder == null && !hasTransfers && maybeExtras?.maybeReply == null &&
    maybeExtras?.maybeForwarded == null && maybeExtras?.maybeLinkPreview == null
  if (isPicturesOnly) {
    // Pictures alone need no coloured frame around them: the picture is the bubble, with the time on it.
    PicturesOnly(images, look, maybeExtras, message, isMine, onOpenFile, onLongPressImage)
    return
  }
  // Short text: the time sits on the text's line instead of a line of its own (the list has it
  // next to the name).
  val isTextOnly = !look.isList && hasText && others.isEmpty() && maybeExtras?.maybeLinkPreview == null && !hasTransfers && message.maybePlaceholder == null
  Surface(color = colors.container, contentColor = colors.content, shape = shape) {
    val mediaPadding = if (look.isList) 0.dp else BUBBLE_MEDIA_PADDING
    Column(Modifier.padding(mediaPadding)) {
      val textPadding = Modifier.padding(horizontal = if (look.isList) 0.dp else BUBBLE_TEXT_PADDING_H - BUBBLE_MEDIA_PADDING)
      if (showsName) {
        Text(
          // A source's name always says it's a bot.
          message.maybeSource?.let { stringResource(R.string.source_sender, it.name) }
            ?: snapshot?.name(message.author).orEmpty(),
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
        ReplyQuote(reply, message.conversation, snapshot, extras, colors, onOpenPicture, Modifier.padding(top = 2.dp).fillMaxWidth())
      }
      if (images.isNotEmpty() && !isDeleted) {
        Box(Modifier.padding(top = if (showsName || maybeExtras?.maybeReply != null) 4.dp else 0.dp)) {
          if (images.size >= 2) ImageGrid(images, look.bubbleMax, onOpenFile, onLongPressImage) else ImagePreview(images.first(), look.bubbleMax, onOpenFile)
        }
      }
      if (!isDeleted) {
        videos.forEach { file ->
          Box(Modifier.padding(top = 4.dp)) { VideoPreview(file, look.bubbleMax, onOpenFile) }
        }
      }
      Column(textPadding.padding(top = if (look.isList) 2.dp else BUBBLE_TEXT_PADDING_V - BUBBLE_MEDIA_PADDING)) {
        when {
          message.maybePlaceholder != null -> PlaceholderLine(message.maybePlaceholder, colors)
          isDeleted -> Text(
            stringResource(R.string.message_deleted),
            style = MaterialTheme.typography.bodyLarge,
            fontStyle = FontStyle.Italic,
            color = colors.muted,
          )
          isTextOnly && images.isEmpty() && videos.isEmpty() -> TextWithTime(
            body = { MessageBody(message.body, colors.content, colors.markup, look.names, onOpenPermalink) },
            time = { Footer(message, maybeExtras, colors, isMine) },
          )
          hasText -> MessageBody(message.body, colors.content, colors.markup, look.names, onOpenPermalink)
        }
        if (!isDeleted) {
          others.forEach { file ->
            if (file.isVoice) VoiceCard(viewModel, file, colors.content, colors.muted, colors.accent) else FileCard(file, colors, onOpenFile)
          }
          maybeExtras?.maybeLinkPreview?.let { LinkPreviewCard(it, colors) }
        }
        files.filter { it.transfer.isCancellable || it.transfer.state == "upload_failed" }.forEach { file ->
          UploadLine(viewModel, file, colors)
        }
        when {
          // The time is next to the name; only markers and the own message's state are left.
          look.isList -> if (hasFooterMarks(message, maybeExtras, isMine)) Footer(message, maybeExtras, colors, isMine, showsTime = false)
          !(isTextOnly && images.isEmpty() && videos.isEmpty()) -> Footer(message, maybeExtras, colors, isMine, Modifier.align(Alignment.End))
        }
      }
    }
  }
}

/**
 * A one-line text with the time at the end of the same line, as in the common messengers; a text
 * of several lines, or one the time doesn't fit next to, gets the time below it, at the end.
 */
@Composable
private fun TextWithTime(body: @Composable () -> Unit, time: @Composable () -> Unit) {
  val lineHeight = with(LocalDensity.current) { MaterialTheme.typography.bodyLarge.lineHeight.toPx() }
  val gap = with(LocalDensity.current) { 8.dp.roundToPx() }
  Layout(content = {
    body()
    time()
  }) { measurables, constraints ->
    val loose = constraints.copy(minWidth = 0)
    val text = measurables[0].measure(loose)
    val footer = measurables[1].measure(loose)
    val isOneLine = text.height < lineHeight * 1.5f
    val sideBySide = text.width + gap + footer.width
    if (isOneLine && sideBySide <= constraints.maxWidth) {
      val height = maxOf(text.height, footer.height)
      layout(maxOf(sideBySide, constraints.minWidth), height) {
        text.placeRelative(0, (height - text.height) / 2)
        footer.placeRelative(sideBySide - footer.width, height - footer.height)
      }
    } else {
      val width = maxOf(text.width, footer.width, constraints.minWidth)
      layout(width, text.height + footer.height) {
        text.placeRelative(0, 0)
        footer.placeRelative(width - footer.width, text.height)
      }
    }
  }
}

/** Pictures without text: no bubble around them, the time on a dark pill in the corner. */
@Composable
private fun PicturesOnly(
  images: List<FileInfo>,
  look: FeedLook,
  maybeExtras: MessageExtras?,
  message: Message,
  isMine: Boolean,
  onOpenFile: (FileInfo) -> Unit,
  onLongPressImage: (FileInfo) -> Unit,
) {
  val onPicture = Color.White
  val overlay = BubbleColors(
    container = Color.Transparent,
    content = onPicture,
    muted = onPicture.copy(alpha = 0.85f),
    accent = onPicture,
    markup = MarkupColors(link = onPicture, mention = onPicture, mentionMe = onPicture, mentionMeBackground = Color.Transparent, codeBackground = Color.Transparent),
  )
  Box {
    if (images.size >= 2) ImageGrid(images, look.bubbleMax, onOpenFile, onLongPressImage) else ImagePreview(images.first(), look.bubbleMax, onOpenFile)
    Footer(
      message,
      maybeExtras,
      overlay,
      isMine,
      Modifier
        .align(Alignment.BottomEnd)
        .padding(6.dp)
        .background(Color.Black.copy(alpha = PICTURE_TIME_SCRIM), RoundedCornerShape(10.dp))
        .padding(horizontal = 6.dp),
    )
  }
}

private const val PICTURE_TIME_SCRIM = 0.45f

/**
 * What a placeholder bubble says (issue #888): waiting with a clock, not available with a lock,
 * failed with a warning. The clock is still: a row may wait for days, and nothing animates while
 * the app idles.
 */
@Composable
private fun PlaceholderLine(placeholder: String, colors: BubbleColors) {
  val (text, icon) = when (placeholder) {
    "waiting" -> stringResource(R.string.message_placeholder_waiting) to painterResource(R.drawable.ic_schedule)
    "failed" -> stringResource(R.string.message_placeholder_failed) to painterResource(R.drawable.ic_warning)
    else -> stringResource(R.string.message_placeholder_not_available) to rememberVectorPainter(Icons.Outlined.Lock)
  }
  Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
    Icon(icon, contentDescription = null, tint = colors.muted, modifier = Modifier.size(16.dp))
    Text(text, style = MaterialTheme.typography.bodyLarge, fontStyle = FontStyle.Italic, color = colors.muted)
  }
}

/**
 * Reactions, read confirmations, "not sent" and the thread link, under the bubble. Without the
 * right to react, the reactions stay readable but don't toggle (issue #758).
 */
@OptIn(ExperimentalLayoutApi::class)
@Composable
private fun BelowBubble(
  viewModel: MainViewModel,
  message: Message,
  maybeExtras: MessageExtras?,
  onOpenThread: ((String) -> Unit)?,
  canReact: Boolean,
  maybeOnAddReaction: (() -> Unit)? = null,
) {
  if (maybeExtras?.isSendFailed == true) NotSentLine(viewModel, message)
  if (message.reactions.isNotEmpty()) {
    FlowRow(
      horizontalArrangement = Arrangement.spacedBy(4.dp),
      verticalArrangement = Arrangement.spacedBy(4.dp),
      modifier = Modifier.padding(top = 2.dp),
    ) {
      for (reaction in message.reactions) {
        ReactionPill(reaction.emoji, reaction.count, reaction.reactedByMe, isEnabled = canReact) {
          viewModel.act(ChatActions.react(message.conversation, message.id, reaction.emoji))
        }
      }
      maybeOnAddReaction?.let { AddReactionPill(it) }
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

/** After the reactions: one more, from the message's menu with every reaction (#1488). */
@Composable
private fun AddReactionPill(onClick: () -> Unit) {
  val scheme = MaterialTheme.colorScheme
  Surface(
    onClick = onClick,
    shape = CircleShape,
    color = scheme.surfaceContainerHigh,
    border = BorderStroke(1.dp, scheme.outlineVariant),
  ) {
    Icon(
      painterResource(R.drawable.ic_face),
      contentDescription = stringResource(R.string.react_add),
      tint = scheme.onSurfaceVariant,
      modifier = Modifier.padding(horizontal = 10.dp, vertical = 3.dp).size(16.dp),
    )
  }
}

/** One reaction: the emoji and how many, outlined in the accent when the user is among them. */
@Composable
private fun ReactionPill(emoji: String, count: Int, isMine: Boolean, isEnabled: Boolean, onClick: () -> Unit) {
  val scheme = MaterialTheme.colorScheme
  Surface(
    onClick = onClick,
    enabled = isEnabled,
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
private fun Footer(
  message: Message,
  maybeExtras: MessageExtras?,
  colors: BubbleColors,
  isMine: Boolean,
  modifier: Modifier = Modifier,
  showsTime: Boolean = true,
) {
  val context = LocalContext.current
  // A rejected message says "not sent" below instead of "sending…" forever.
  val isSending = message.isPending && maybeExtras?.isSendFailed != true
  Row(
    modifier.padding(top = 2.dp, bottom = 2.dp),
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
    if (showsTime) FooterLabel(DateUtils.formatDateTime(context, message.sentAt, DateUtils.FORMAT_SHOW_TIME), colors.muted)
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
      // The other member of the DM read it: two ticks at the bubble's full text colour, where the
      // single "sent" tick is muted (desktop and web: two accent ticks, #769).
      maybeExtras?.isRead == true -> Icon(
        painterResource(R.drawable.ic_done_all),
        contentDescription = stringResource(R.string.message_read),
        tint = colors.content,
        modifier = Modifier.size(14.dp),
      )
      else -> Icon(Icons.Filled.Done, contentDescription = stringResource(R.string.message_sent), tint = colors.muted, modifier = Modifier.size(14.dp))
    }
  }
}

/**
 * Whether the list's footer has anything to say: pinned, priority, edited, or an own message that
 * is still sending, failed or was read. The plain "sent" tick alone isn't worth a line.
 */
private fun hasFooterMarks(message: Message, maybeExtras: MessageExtras?, isMine: Boolean): Boolean =
  (isMine && (message.isPending || maybeExtras?.isSendFailed == true || maybeExtras?.isRead == true)) || message.isEdited || maybeExtras?.isPinned == true || maybeExtras?.priority == "urgent" || maybeExtras?.priority == "important"

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

/** A round avatar: the sender's picture, or their initial on their hue. */
@Composable
private fun SenderAvatar(maybeUser: User?, name: String, message: Message, size: Dp = AVATAR_SIZE) {
  val account: @Composable () -> Unit = {
    UserAvatar(maybeUser?.id, Modifier.size(size)) {
      Box(
        Modifier.size(size).background(avatarColorOfHue(maybeUser?.avatarHue ?: 0), CircleShape),
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
  }
  val maybeSourcePicture = message.maybeSource?.maybePictureSha256
  if (maybeSourcePicture != null) {
    SourceAvatar(message.conversation, maybeSourcePicture, Modifier.size(size), account)
  } else {
    account()
  }
}

private const val NAME_SATURATION = 0.55f
private const val NAME_LIGHTNESS_LIGHT = 0.38f
private const val NAME_LIGHTNESS_DARK = 0.72f

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
        // A failed attempt says so at once, not only after the core gives up (#1355): a ring that
        // keeps turning without a word reads as stuck.
        if (transfer.isRetrying) {
          Text(
            label,
            color = colors.content,
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.padding(start = 6.dp).weight(1f, fill = false),
          )
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
private fun ReplyQuote(
  reply: ReplyPreview,
  conversation: String,
  snapshot: ChatSnapshot?,
  extras: Map<String, MessageExtras>,
  colors: BubbleColors,
  onOpenPicture: (PictureTarget) -> Unit,
  modifier: Modifier,
) {
  val maybeQuoted = reply.maybeAttachment
  val maybeImage = reply.maybeMessage?.takeIf { maybeQuoted == null }?.let { extras[it] }?.files?.firstOrNull { it.isImage && it.maybeThumbnail != null }
  val maybeTarget = maybeQuoted?.takeIf { it.canOpen }?.let { quoted ->
    reply.maybeMessage?.let { message -> PictureTarget(conversation, message, quoted.fileId) }
  }
  val openLabel = stringResource(R.string.file_open)
  Row(
    modifier
      .clip(RoundedCornerShape(BUBBLE_CORNER_INNER))
      .background(colors.content.copy(alpha = QUOTE_BACKGROUND_ALPHA))
      .then(if (maybeTarget != null) Modifier.clickable(onClickLabel = openLabel) { onOpenPicture(maybeTarget) } else Modifier)
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
    maybeQuoted?.let { quoted -> QuotedPicture(quoted, colors) }
  }
}

/** The thumbnail a reply carries of the picture it answers; it outlives the picture (#241). */
@Composable
private fun QuotedPicture(quoted: ReplyAttachmentPreview, colors: BubbleColors) {
  val maybeBitmap by rememberThumbnail("quote:${quoted.fileId}", quoted.maybeThumbnail?.bytes, THUMBNAIL_DECODE_PX)
  Box(Modifier.padding(4.dp).size(40.dp).clip(RoundedCornerShape(6.dp)), contentAlignment = Alignment.Center) {
    val bitmap = maybeBitmap
    if (bitmap != null) {
      Image(bitmap, contentDescription = quoted.fileName, contentScale = ContentScale.Crop, modifier = Modifier.fillMaxSize())
    } else {
      Icon(painterResource(R.drawable.ic_image), contentDescription = quoted.fileName, tint = colors.muted)
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
    TransferOverlay(file) { TilePicture(file) }
  }
}

/** A video's poster at its own shape, with a play mark, its length and its size; a tap opens the player. */
@Composable
private fun VideoPreview(file: FileInfo, bubbleMax: Dp, onOpen: (FileInfo) -> Unit) {
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
      .clickable(onClickLabel = stringResource(R.string.video_play)) { onOpen(file) },
  ) {
    TransferOverlay(file) { Thumbnail(file, ContentScale.Crop) }
    PlayMark(Modifier.align(Alignment.Center))
    VideoBadge(file, Modifier.align(Alignment.BottomStart))
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

/**
 * A message's pictures as an album across the bubble, laid out as on every platform (the core's
 * `albumLayout`, #240): two side by side, three with the first spanning both rows, four in two
 * rows, and "+N" on the last tile for the rest. A tap opens the viewer on that picture; it pages
 * through all of them.
 */
@Composable
private fun ImageGrid(images: List<FileInfo>, bubbleMax: Dp, onOpen: (FileInfo) -> Unit, onLongPress: (FileInfo) -> Unit) {
  val layout = remember(images.size) { albumLayout(images.size.toUInt()) }
    ?: AlbumLayout(tiles = images.size.toUInt(), hidden = 0u, isFirstTall = false)
  val shown = images.take(layout.tiles.toInt())
  val width = bubbleMax - BUBBLE_MEDIA_PADDING * 2
  val side = (width - ALBUM_GAP) / 2
  fun hiddenAt(index: Int): Int? = if (index == shown.lastIndex && layout.hidden > 0u) layout.hidden.toInt() else null
  if (layout.isFirstTall && shown.size == 3) {
    Row(Modifier.width(width), horizontalArrangement = Arrangement.spacedBy(ALBUM_GAP)) {
      AlbumTile(shown[0], null, Modifier.width(side).height(side * 2 + ALBUM_GAP), onOpen, onLongPress)
      Column(verticalArrangement = Arrangement.spacedBy(ALBUM_GAP)) {
        AlbumTile(shown[1], null, Modifier.size(side), onOpen, onLongPress)
        AlbumTile(shown[2], hiddenAt(2), Modifier.size(side), onOpen, onLongPress)
      }
    }
  } else {
    Column(Modifier.width(width), verticalArrangement = Arrangement.spacedBy(ALBUM_GAP)) {
      for ((row, pair) in shown.chunked(2).withIndex()) {
        Row(horizontalArrangement = Arrangement.spacedBy(ALBUM_GAP)) {
          pair.forEachIndexed { column, image -> AlbumTile(image, hiddenAt(row * 2 + column), Modifier.size(side), onOpen, onLongPress) }
        }
      }
    }
  }
}

/** One tile of an album; on the last one, the pictures it doesn't show ("+3"). */
@Composable
private fun AlbumTile(
  image: FileInfo,
  maybeHidden: Int?,
  modifier: Modifier,
  onOpen: (FileInfo) -> Unit,
  onLongPress: (FileInfo) -> Unit,
) {
  val more = maybeHidden?.let { pluralStringResource(R.plurals.album_more_pictures, it, it) }
  val haptics = LocalHapticFeedback.current
  Box(
    modifier
      .clip(RoundedCornerShape(MEDIA_CORNER))
      .background(MaterialTheme.colorScheme.surfaceVariant)
      .combinedClickable(
        onClickLabel = stringResource(R.string.file_open),
        onLongClickLabel = stringResource(R.string.more),
        onClick = { onOpen(image) },
        onLongClick = {
          haptics.performHapticFeedback(HapticFeedbackType.LongPress)
          onLongPress(image)
        },
      )
      .semantics { more?.let { stateDescription = it } },
  ) {
    TransferOverlay(image) { TilePicture(image) }
    maybeHidden?.let { hidden ->
      Box(Modifier.fillMaxSize().background(Color.Black.copy(alpha = ALBUM_MORE_SCRIM)), contentAlignment = Alignment.Center) {
        Text(
          "+$hidden",
          color = Color.White,
          style = MaterialTheme.typography.headlineSmall,
          fontWeight = FontWeight.SemiBold,
          modifier = Modifier.clearAndSetSemantics {},
        )
      }
    }
  }
}

/** The gap between an album's tiles, and the shade under "+N". */
private val ALBUM_GAP = 3.dp
private const val ALBUM_MORE_SCRIM = 0.45f

/**
 * A picture of the conversation: the thumbnail, or for an animated image what moves, with its
 * button and badge (#201). A tap on the button stops or starts it; a tap elsewhere opens the viewer
 * as on any picture, and a long press stays the menu.
 */
@Composable
private fun BoxScope.TilePicture(file: FileInfo) {
  if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P || !mayBeAnimated(file.mediaType)) {
    Thumbnail(file, ContentScale.Crop)
    return
  }
  val motion = LocalFeedMotion.current
  val isResumed = isAppResumed()
  var choice by rememberSaveable(file.fileId) { mutableStateOf(PlayChoice.UNSET) }
  val isWanted = wantsToMove(motion.mode, choice)
  val isKnownAnimation = isAnimationType(file.mediaType)
  // A file that is still here only needs reading once it is to move, or when only reading says
  // whether it moves at all.
  val maybePath = file.transfer.path?.takeIf { file.transfer.state == "local" && (isWanted || choice == PlayChoice.PAUSED || !isKnownAnimation) }
  val read = rememberAnimation(maybePath, THUMBNAIL_DECODE_PX)
  val maybeAnimation = (read as? AnimationRead.Moving)?.drawable
  // A file that turned out not to move here has nothing to play.
  val canMoveHere = if (isKnownAnimation) read !is AnimationRead.Still else maybeAnimation != null
  val isOnView = isResumed && !motion.isCovered
  // Cancelled when the row leaves, so a row scrolled past fetches nothing; a fetch once started runs on.
  LaunchedEffect(file.fileId, file.transfer.state, isWanted, isOnView) {
    val isAutomatic = choice == PlayChoice.UNSET && file.sizeBytes <= MAX_AUTO_ANIMATION_BYTES
    if (!isKnownAnimation || !isWanted || !isOnView || !isAutomatic || file.transfer.state != "remote") return@LaunchedEffect
    delay(AUTO_FETCH_SETTLE_MS)
    if (motion.fetchesNow() && animationFetchBudget.take(file.sizeBytes)) motion.fetch(file)
  }
  val isMoving = maybeAnimation != null && isWanted && isOnView
  if (maybeAnimation != null) {
    AnimationView(
      maybeAnimation,
      isPlaying = isMoving,
      contentScale = ContentScale.Crop,
      modifier = Modifier.fillMaxSize(),
      onEnded = { choice = PlayChoice.PAUSED },
    )
  } else {
    Thumbnail(file, ContentScale.Crop)
  }
  if (!canMoveHere) return
  val hasFailed = file.transfer.state == "failed"
  if (!isMoving) {
    AnimationBadge(
      if (hasFailed) stringResource(R.string.animation_failed) else animationLabel(file.mediaType),
      Modifier.align(Alignment.BottomStart),
    )
  }
  val stateLabel = stringResource(if (isMoving) R.string.animation_state_playing else R.string.animation_state_paused)
  AnimationButton(
    isMoving = isMoving || (isWanted && file.transfer.isMoving),
    onClick = {
      val wasGoing = isMoving || (isWanted && file.transfer.isMoving)
      choice = if (wasGoing) PlayChoice.PAUSED else PlayChoice.PLAYING
      // Asked for by hand: on any network and at any size.
      if (!wasGoing && (file.transfer.state == "remote" || hasFailed)) motion.fetch(file)
    },
    modifier = Modifier.align(Alignment.TopStart).semantics { stateDescription = stateLabel },
  )
}

/** A preview's picture, decoded in the background; the tile's own color stands in until then. */
@Composable
internal fun Thumbnail(file: FileInfo, contentScale: ContentScale) {
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
