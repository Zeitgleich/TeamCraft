package de.zeitgleich.teamcraft.ui.media

import android.content.Context
import android.graphics.Bitmap
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.text.format.DateUtils
import android.text.format.Formatter
import androidx.annotation.OptIn
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.gestures.detectHorizontalDragGestures
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.produceState
import androidx.compose.runtime.rememberUpdatedState
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.ProgressBarRangeInfo
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.progressBarRangeInfo
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.setProgress
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.dp
import androidx.media3.common.MediaItem
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.effect.Presentation
import androidx.media3.transformer.Composition
import androidx.media3.transformer.DefaultEncoderFactory
import androidx.media3.transformer.EditedMediaItem
import androidx.media3.transformer.Effects
import androidx.media3.transformer.ExportException
import androidx.media3.transformer.ExportResult
import androidx.media3.transformer.Transformer
import androidx.media3.transformer.VideoEncoderSettings
import java.io.File
import kotlin.coroutines.resume
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import de.zeitgleich.teamcraft.R

/** The shortest part of a video that can be kept. */
const val MIN_TRIM_MS = 1000L

/** How many frames the trim bar shows. */
private const val TRIM_FRAMES = 8

/** Standard quality: the shorter side and the bit rates a phone video is brought down to. */
private const val STANDARD_SHORT_SIDE_PX = 720
private const val STANDARD_VIDEO_BITRATE = 2_000_000
private const val STANDARD_AUDIO_BITRATE = 128_000

/** A video above this bit rate (or larger than the short side) is re-encoded for Standard. */
private const val STANDARD_MAX_SOURCE_BITRATE = 3_000_000

/** What the device knows about a video without decoding it. */
data class VideoFacts(val durationMs: Long, val shortSidePx: Int, val bitrate: Int, val sizeBytes: Long)

/**
 * Trims, mutes and scales videos on the device before sending (#1358), with Media3 Transformer.
 * Trim and mute alone keep the encoded video where the container allows it (only the start is
 * re-encoded); Standard quality re-encodes to 720p. The video is streamed from and to files; it is
 * never held in memory whole.
 */
object VideoEditing {
  suspend fun facts(context: Context, uri: Uri): VideoFacts? = withContext(Dispatchers.IO) {
    runCatching {
      val retriever = MediaMetadataRetriever()
      try {
        retriever.setDataSource(context, uri)
        fun number(key: Int) = retriever.extractMetadata(key)?.toLongOrNull() ?: 0L
        val width = number(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH).toInt()
        val height = number(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT).toInt()
        val size = context.contentResolver.openAssetFileDescriptor(uri, "r")?.use { it.length } ?: 0L
        VideoFacts(number(MediaMetadataRetriever.METADATA_KEY_DURATION), minOf(width, height), number(MediaMetadataRetriever.METADATA_KEY_BITRATE).toInt(), size)
      } finally {
        retriever.release()
      }
    }.getOrNull()
  }

  /** Whether sending this video at this quality needs Transformer, or the file can go as it is. */
  fun needsExport(facts: VideoFacts?, edit: VideoEdit, quality: MediaQuality): Boolean {
    if (facts == null) return edit.isEdited(Long.MAX_VALUE)
    return edit.isEdited(facts.durationMs) || (quality == MediaQuality.Standard && isLargerThanStandard(facts))
  }

  /** About how large the sent video will be, for the trim bar while trimming. */
  fun estimatedBytes(facts: VideoFacts, edit: VideoEdit, quality: MediaQuality): Long {
    val keptMs = (edit.endMs(facts.durationMs) - edit.startMs).coerceAtLeast(0)
    val share = if (facts.durationMs > 0) keptMs.toDouble() / facts.durationMs else 1.0
    return if (quality == MediaQuality.Standard && isLargerThanStandard(facts)) {
      val audio = if (edit.isMuted) 0 else STANDARD_AUDIO_BITRATE
      (STANDARD_VIDEO_BITRATE + audio).toLong() * keptMs / 8000
    } else {
      (facts.sizeBytes * share).toLong()
    }
  }

  /** Exports the edited video into `target`; `false` when the device can't (the user is told). */
  @OptIn(UnstableApi::class)
  suspend fun export(context: Context, uri: Uri, edit: VideoEdit, facts: VideoFacts?, quality: MediaQuality, target: File): Boolean =
    withContext(Dispatchers.Main) {
      val clipping = MediaItem.ClippingConfiguration.Builder()
        .setStartPositionMs(edit.startMs)
        .apply { edit.maybeEndMs?.let { setEndPositionMs(it) } }
        .build()
      val item = MediaItem.Builder().setUri(uri).setClippingConfiguration(clipping).build()
      val isScaling = quality == MediaQuality.Standard && facts != null && isLargerThanStandard(facts)
      val effects = if (isScaling) Effects(emptyList(), listOf(Presentation.createForShortSide(STANDARD_SHORT_SIDE_PX))) else Effects.EMPTY
      val edited = EditedMediaItem.Builder(item).setRemoveAudio(edit.isMuted).setEffects(effects).build()
      suspendCancellableCoroutine { continuation ->
        val builder = Transformer.Builder(context)
          .addListener(object : Transformer.Listener {
            override fun onCompleted(composition: Composition, exportResult: ExportResult) {
              if (continuation.isActive) continuation.resume(true)
            }

            override fun onError(composition: Composition, exportResult: ExportResult, exportException: ExportException) {
              target.delete()
              if (continuation.isActive) continuation.resume(false)
            }
          })
        if (isScaling) {
          builder.setVideoMimeType(MimeTypes.VIDEO_H264)
          builder.setEncoderFactory(
            DefaultEncoderFactory.Builder(context)
              .setRequestedVideoEncoderSettings(VideoEncoderSettings.Builder().setBitrate(STANDARD_VIDEO_BITRATE).build())
              .build(),
          )
        } else {
          // Only the part before the first kept key frame is re-encoded; the rest is copied.
          builder.experimentalSetTrimOptimizationEnabled(true)
        }
        val transformer = builder.build()
        continuation.invokeOnCancellation {
          transformer.cancel()
          target.delete()
        }
        transformer.start(edited, target.absolutePath)
      }
    }

  private fun isLargerThanStandard(facts: VideoFacts): Boolean =
    facts.shortSidePx > STANDARD_SHORT_SIDE_PX || facts.bitrate > STANDARD_MAX_SOURCE_BITRATE
}

/** One frame of a video at about `sidePx`, for thumbnails and the trim bar. */
suspend fun videoFrame(context: Context, uri: Uri, timeMs: Long, sidePx: Int): Bitmap? = withContext(Dispatchers.IO) {
  runCatching {
    val retriever = MediaMetadataRetriever()
    try {
      retriever.setDataSource(context, uri)
      val timeUs = timeMs * 1000
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O_MR1) {
        retriever.getScaledFrameAtTime(timeUs, MediaMetadataRetriever.OPTION_CLOSEST_SYNC, sidePx, sidePx)
      } else {
        retriever.getFrameAtTime(timeUs, MediaMetadataRetriever.OPTION_CLOSEST_SYNC)
      }
    } finally {
      retriever.release()
    }
  }.getOrNull()
}

/**
 * The trim bar: the video's frames in a row with a handle at each end; the part outside is dimmed.
 * The kept length and the size it will have show above while trimming.
 */
@Composable
fun VideoTrimBar(uri: Uri, facts: VideoFacts, edit: VideoEdit, quality: MediaQuality, onEdit: (VideoEdit) -> Unit, modifier: Modifier = Modifier) {
  val context = LocalContext.current
  val duration = facts.durationMs.coerceAtLeast(1)
  val heightPx = with(LocalDensity.current) { TRIM_HEIGHT.roundToPx() }
  val frames by produceState<List<ImageBitmap>>(emptyList(), uri) {
    value = (0 until TRIM_FRAMES).mapNotNull { index ->
      videoFrame(context, uri, duration * index / TRIM_FRAMES, heightPx * 2)?.asImageBitmap()
    }
  }
  val current by rememberUpdatedState(edit)
  val start = edit.startMs
  val end = edit.endMs(duration)
  Column(modifier) {
    val length = DateUtils.formatElapsedTime((end - start) / 1000)
    val size = Formatter.formatShortFileSize(context, VideoEditing.estimatedBytes(facts, edit, quality))
    Text(stringResource(R.string.media_video_length, length, size), color = Color.White, style = MaterialTheme.typography.labelMedium, modifier = Modifier.align(Alignment.CenterHorizontally).padding(bottom = 6.dp))
    BoxWithConstraints(Modifier.fillMaxWidth().height(TRIM_HEIGHT)) {
      val widthPx = constraints.maxWidth.toFloat()
      val handlePx = with(LocalDensity.current) { HANDLE_WIDTH.toPx() }
      val trackPx = (widthPx - handlePx * 2).coerceAtLeast(1f)
      fun msToPx(ms: Long) = handlePx + trackPx * ms / duration
      Row(Modifier.fillMaxSize().padding(horizontal = HANDLE_WIDTH).clip(RoundedCornerShape(4.dp))) {
        for (frame in frames) {
          Image(frame, contentDescription = null, contentScale = ContentScale.Crop, modifier = Modifier.weight(1f).fillMaxHeight())
        }
      }
      val startPx = msToPx(start)
      val endPx = msToPx(end)
      // Dimmed outside the kept part.
      Box(Modifier.offset { IntOffset(handlePx.roundToInt(), 0) }.width(with(LocalDensity.current) { (startPx - handlePx).coerceAtLeast(0f).toDp() }).fillMaxHeight().background(DIM))
      Box(Modifier.offset { IntOffset(endPx.roundToInt(), 0) }.width(with(LocalDensity.current) { (widthPx - handlePx - endPx).coerceAtLeast(0f).toDp() }).fillMaxHeight().background(DIM))
      // The kept part's frame.
      Box(
        Modifier
          .offset { IntOffset((startPx - handlePx).roundToInt(), 0) }
          .width(with(LocalDensity.current) { (endPx - startPx + handlePx * 2).toDp() })
          .fillMaxHeight()
          .border(2.dp, HANDLE_COLOR, RoundedCornerShape(6.dp)),
      )
      val startLabel = stringResource(R.string.media_trim_start)
      val endLabel = stringResource(R.string.media_trim_end)
      TrimHandle(
        offsetPx = startPx - handlePx,
        label = startLabel,
        valueMs = start,
        durationMs = duration,
        onSet = { onEdit(current.copy(startMs = it.coerceIn(0, current.endMs(duration) - MIN_TRIM_MS))) },
        onDrag = { dx ->
          val ms = current.startMs + (dx / trackPx * duration).toLong()
          onEdit(current.copy(startMs = ms.coerceIn(0, current.endMs(duration) - MIN_TRIM_MS)))
        },
      )
      TrimHandle(
        offsetPx = endPx,
        label = endLabel,
        valueMs = end,
        durationMs = duration,
        onSet = { ms -> onEdit(current.copy(maybeEndMs = ms.coerceIn(current.startMs + MIN_TRIM_MS, duration).takeIf { it < duration })) },
        onDrag = { dx ->
          val ms = current.endMs(duration) + (dx / trackPx * duration).toLong()
          onEdit(current.copy(maybeEndMs = ms.coerceIn(current.startMs + MIN_TRIM_MS, duration).takeIf { it < duration }))
        },
      )
    }
  }
}

@Composable
private fun TrimHandle(offsetPx: Float, label: String, valueMs: Long, durationMs: Long, onSet: (Long) -> Unit, onDrag: (Float) -> Unit) {
  Box(
    Modifier
      .offset { IntOffset(offsetPx.roundToInt(), 0) }
      .width(HANDLE_WIDTH)
      .fillMaxHeight()
      .background(HANDLE_COLOR, RoundedCornerShape(6.dp))
      .semantics {
        contentDescription = label
        progressBarRangeInfo = ProgressBarRangeInfo(valueMs.toFloat(), 0f..durationMs.toFloat())
        setProgress { target -> onSet(target.toLong()); true }
      }
      .pointerInput(Unit) { detectHorizontalDragGestures { change, amount -> change.consume(); onDrag(amount) } },
    contentAlignment = Alignment.Center,
  ) {
    Box(Modifier.width(3.dp).height(18.dp).background(Color.Black.copy(alpha = 0.6f), RoundedCornerShape(2.dp)))
  }
}

private val TRIM_HEIGHT = 52.dp
private val HANDLE_WIDTH = 14.dp
private val HANDLE_COLOR = Color(0xFFFFFFFF)
private val DIM = Color.Black.copy(alpha = 0.6f)
