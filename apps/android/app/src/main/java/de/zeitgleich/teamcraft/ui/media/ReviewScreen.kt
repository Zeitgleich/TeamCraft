package de.zeitgleich.teamcraft.ui.media

import android.net.Uri
import android.os.Build
import androidx.annotation.OptIn
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawing
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Send
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledIconButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.flow.distinctUntilChanged
import de.zeitgleich.teamcraft.R

/**
 * Review and send (#1358): dark, the conversation's name and "1 of 2" on top, the items as a pager,
 * a rail of thumbnails with remove on the current one, the tools (crop, draw, mute for videos,
 * quality, save to the device, add more), and the caption with send at the bottom.
 */
@Composable
fun ReviewScreen(
  draft: MediaDraft,
  conversationName: String,
  isSending: Boolean,
  onDraft: (MediaDraft) -> Unit,
  onCrop: () -> Unit,
  onDraw: () -> Unit,
  onSave: () -> Unit,
  onAddMore: () -> Unit,
  onBack: () -> Unit,
  onSend: () -> Unit,
) {
  val items = draft.items
  val pager = rememberPagerState(initialPage = draft.currentIndex) { items.size }
  val latest = androidx.compose.runtime.rememberUpdatedState(draft)
  LaunchedEffect(pager) {
    snapshotFlow { pager.currentPage }.distinctUntilChanged().collect { page ->
      if (latest.value.currentIndex != page) onDraft(latest.value.copy(currentIndex = page))
    }
  }
  LaunchedEffect(draft.currentIndex, items.size) {
    if (draft.currentIndex in items.indices && pager.currentPage != draft.currentIndex) pager.animateScrollToPage(draft.currentIndex)
  }
  val maybeCurrent = draft.maybeCurrent
  // The trim bar shows on demand, as in the common messengers.
  var trimmingId by remember { mutableStateOf<String?>(null) }
  Column(Modifier.fillMaxSize().background(Color.Black).windowInsetsPadding(WindowInsets.safeDrawing).imePadding()) {
    Row(Modifier.fillMaxWidth().padding(horizontal = 4.dp, vertical = 4.dp), verticalAlignment = Alignment.CenterVertically) {
      IconButton(onClick = onBack) {
        Icon(painterResource(R.drawable.ic_arrow_back), contentDescription = stringResource(R.string.media_back), tint = Color.White)
      }
      Column(Modifier.weight(1f).padding(start = 4.dp)) {
        Text(conversationName, color = Color.White, style = MaterialTheme.typography.titleMedium, maxLines = 1, overflow = TextOverflow.Ellipsis)
        if (items.size > 1) {
          Text(stringResource(R.string.media_position, draft.currentIndex + 1, items.size), color = Color.White.copy(alpha = 0.7f), style = MaterialTheme.typography.bodySmall)
        }
      }
    }
    HorizontalPager(pager, modifier = Modifier.weight(1f).fillMaxWidth(), key = { items.getOrNull(it)?.id ?: it }) { page ->
      val item = items.getOrNull(page) ?: return@HorizontalPager
      if (item.isVideo) {
        VideoPage(item, isCurrent = page == pager.currentPage, isTrimming = trimmingId == item.id, quality = draft.quality, onEdit = { edit -> onDraft(draft.updated(item.id) { it.copy(video = edit) }) })
      } else {
        PicturePage(item)
      }
    }
    if (items.size > 1) ThumbnailRail(draft, onSelect = { onDraft(draft.copy(currentIndex = it)) }, onRemove = { id -> onDraft(draft.removed(id)) })
    Row(Modifier.fillMaxWidth().padding(horizontal = 8.dp), horizontalArrangement = Arrangement.SpaceEvenly) {
      if (maybeCurrent != null && !maybeCurrent.isVideo) {
        ToolButton(R.drawable.ic_crop_rotate, R.string.media_tool_crop, onClick = onCrop)
        ToolButton(R.drawable.ic_draw, R.string.media_tool_draw, onClick = onDraw)
      }
      if (maybeCurrent != null && maybeCurrent.isVideo) {
        ToolButton(
          R.drawable.ic_content_cut,
          R.string.media_tool_trim,
          // Lit while open, and while the video is shortened.
          isActive = trimmingId == maybeCurrent.id || maybeCurrent.video.startMs > 0 || maybeCurrent.video.maybeEndMs != null,
          onClick = { trimmingId = if (trimmingId == maybeCurrent.id) null else maybeCurrent.id },
        )
        val isMuted = maybeCurrent.video.isMuted
        ToolButton(
          if (isMuted) R.drawable.ic_volume_off else R.drawable.ic_volume_up,
          if (isMuted) R.string.media_unmute else R.string.media_mute,
          isActive = isMuted,
          onClick = { onDraft(draft.updated(maybeCurrent.id) { it.copy(video = it.video.copy(isMuted = !isMuted)) }) },
        )
      }
      val isHigh = draft.quality == MediaQuality.High
      ToolButton(
        if (isHigh) R.drawable.ic_hd else R.drawable.ic_sd,
        if (isHigh) R.string.media_quality_high else R.string.media_quality_standard,
        onClick = { onDraft(draft.copy(quality = if (isHigh) MediaQuality.Standard else MediaQuality.High)) },
      )
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ToolButton(R.drawable.ic_download, R.string.media_save, onClick = onSave)
      ToolButton(R.drawable.ic_add_photo_alternate, R.string.media_add_more, onClick = onAddMore)
    }
    if (maybeCurrent?.isVideo == true && maybeCurrent.video.isMuted) {
      Text(stringResource(R.string.media_muted_note), color = Color.White.copy(alpha = 0.7f), style = MaterialTheme.typography.bodySmall, modifier = Modifier.align(Alignment.CenterHorizontally))
    }
    CaptionRow(draft.caption, isSending, onCaption = { onDraft(draft.copy(caption = it)) }, onSend = onSend, canSend = items.isNotEmpty())
  }
}

@Composable
private fun ToolButton(icon: Int, label: Int, isActive: Boolean = false, onClick: () -> Unit) {
  IconButton(onClick = onClick) {
    Icon(painterResource(icon), contentDescription = stringResource(label), tint = if (isActive) ACTIVE_TINT else Color.White)
  }
}

/** A picture as it will be sent, edits included. */
@Composable
private fun PicturePage(item: MediaDraftItem) {
  val maybePicture by rememberEditablePicture(item.uri)
  BoxWithConstraints(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
    val picture = maybePicture
    if (picture == null) {
      CircularProgressIndicator(color = Color.White)
    } else {
      val fit = PictureFit.of(picture, item.picture.crop, isFramed = true, constraints.maxWidth.toFloat(), constraints.maxHeight.toFloat(), 0f)
      EditedPictureCanvas(picture, item.picture, fit, Modifier.fillMaxSize())
    }
  }
}

/** A video playing the part that will be sent (silent when muted), with the trim bar under it. */
@OptIn(UnstableApi::class)
@Composable
private fun VideoPage(item: MediaDraftItem, isCurrent: Boolean, isTrimming: Boolean, quality: MediaQuality, onEdit: (VideoEdit) -> Unit) {
  val context = LocalContext.current
  val uri = remember(item.uri) { Uri.parse(item.uri) }
  val maybeFacts by produceState<VideoFacts?>(null, uri) { value = VideoEditing.facts(context, uri) }
  Column(Modifier.fillMaxSize()) {
    Box(Modifier.weight(1f).fillMaxWidth()) {
      if (isCurrent) {
        val player = remember(uri) {
          // The camera's videos are sealed: played from the sealed file, decrypted as it reads.
          val sources = DefaultMediaSourceFactory(SealedCaptures.Factory(context, DefaultDataSource.Factory(context)))
          ExoPlayer.Builder(context).setMediaSourceFactory(sources).build().apply { repeatMode = Player.REPEAT_MODE_ALL }
        }
        DisposableEffect(player) { onDispose { player.release() } }
        val start = item.video.startMs
        val maybeEnd = item.video.maybeEndMs
        LaunchedEffect(player, start, maybeEnd) {
          val clipping = MediaItem.ClippingConfiguration.Builder().setStartPositionMs(start).apply { maybeEnd?.let { setEndPositionMs(it) } }.build()
          player.setMediaItem(MediaItem.Builder().setUri(uri).setClippingConfiguration(clipping).build())
          player.prepare()
        }
        LaunchedEffect(player, item.video.isMuted) { player.volume = if (item.video.isMuted) 0f else 1f }
        AndroidView(
          factory = { PlayerView(it).apply { this.player = player; setShowNextButton(false); setShowPreviousButton(false) } },
          update = { it.player = player },
          modifier = Modifier.fillMaxSize(),
        )
      }
    }
    maybeFacts?.takeIf { isTrimming }?.let { facts ->
      VideoTrimBar(uri, facts, item.video, quality, onEdit, Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp))
    }
  }
}

@Composable
private fun ThumbnailRail(draft: MediaDraft, onSelect: (Int) -> Unit, onRemove: (String) -> Unit) {
  val state = rememberLazyListState()
  LaunchedEffect(draft.currentIndex) { state.animateScrollToItem(draft.currentIndex.coerceAtLeast(0)) }
  val removeLabel = stringResource(R.string.media_remove_item)
  LazyRow(
    state = state,
    contentPadding = PaddingValues(horizontal = 16.dp, vertical = 8.dp),
    horizontalArrangement = Arrangement.spacedBy(8.dp),
    modifier = Modifier.fillMaxWidth(),
  ) {
    itemsIndexed(draft.items, key = { _, item -> item.id }) { index, item ->
      val isCurrent = index == draft.currentIndex
      Box(
        Modifier
          .size(56.dp)
          .clip(RoundedCornerShape(10.dp))
          .border(if (isCurrent) 2.dp else 0.dp, Color.White, RoundedCornerShape(10.dp))
          .clickable { if (isCurrent) onRemove(item.id) else onSelect(index) }
          .semantics { if (isCurrent) contentDescription = removeLabel },
      ) {
        Thumbnail(item.asRecent(), side = 56.dp, modifier = Modifier.matchParentSize())
        if (isCurrent) {
          // The current one's remove: the whole tile, marked with a bin.
          Box(Modifier.matchParentSize().background(Color.Black.copy(alpha = 0.45f)), contentAlignment = Alignment.Center) {
            Icon(painterResource(R.drawable.ic_delete), contentDescription = null, tint = Color.White)
          }
        } else if (item.isVideo) {
          VideoLength(item.durationMs, Modifier.align(Alignment.BottomStart))
        }
      }
    }
  }
}

@Composable
private fun CaptionRow(caption: String, isSending: Boolean, onCaption: (String) -> Unit, onSend: () -> Unit, canSend: Boolean) {
  Row(Modifier.fillMaxWidth().padding(start = 12.dp, end = 8.dp, top = 8.dp, bottom = 8.dp), verticalAlignment = Alignment.CenterVertically) {
    Box(
      Modifier.weight(1f).heightIn(min = 44.dp).background(Color.White.copy(alpha = 0.12f), RoundedCornerShape(22.dp)).padding(horizontal = 16.dp, vertical = 11.dp),
      contentAlignment = Alignment.CenterStart,
    ) {
      if (caption.isEmpty()) Text(stringResource(R.string.media_caption), color = Color.White.copy(alpha = 0.6f), style = MaterialTheme.typography.bodyLarge)
      val label = stringResource(R.string.media_caption)
      BasicTextField(
        value = caption,
        onValueChange = onCaption,
        enabled = !isSending,
        textStyle = MaterialTheme.typography.bodyLarge.merge(TextStyle(color = Color.White)),
        cursorBrush = SolidColor(Color.White),
        maxLines = 5,
        modifier = Modifier.fillMaxWidth().semantics { contentDescription = label },
      )
    }
    FilledIconButton(onClick = onSend, enabled = canSend && !isSending, modifier = Modifier.padding(start = 8.dp).size(48.dp), shape = CircleShape) {
      if (isSending) {
        CircularProgressIndicator(Modifier.size(20.dp), strokeWidth = 2.dp, color = MaterialTheme.colorScheme.onPrimary)
      } else {
        Icon(Icons.AutoMirrored.Filled.Send, contentDescription = stringResource(R.string.send))
      }
    }
  }
}
